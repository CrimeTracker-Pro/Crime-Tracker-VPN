use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};
use utoipa::ToSchema;
use zerovpn_auth::totp;
use zerovpn_db::repos::{audit, session_events, users};
use zerovpn_wg::qr;

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::CurrentUser,
    routes::dto::StatusAck,
    state::AppState,
};

const ISSUER: &str = "Crime Tracker VPN";

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupResponse {
    /// Plaintext base32 secret (shown once for manual entry).
    pub secret: String,
    /// otpauth:// URI for QR scanning.
    pub provisioning_uri: String,
    /// SVG of the URI as a QR code.
    pub qr_svg: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnableBody {
    pub secret: String,
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnableResponse {
    /// Plaintext recovery codes — shown once. After this, only hashes survive.
    pub recovery_codes: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyBody {
    pub code: String,
}

/// Generate a fresh TOTP secret + QR. Doesn't persist anything; the user
/// must call `enable` with the same secret + a working code to commit.
#[utoipa::path(
    post,
    path = "/auth/totp/setup",
    tag = "Auth",
    responses(
        (status = 200, description = "Provisional TOTP material — call /auth/totp/enable to commit", body = SetupResponse),
        (status = 409, description = "2FA already enabled"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn setup(
    State(_state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    if user.totp_enabled {
        return Err(ApiError::Conflict("2FA already enabled".into()));
    }
    let secret = totp::generate_secret_b32();
    let uri = totp::provisioning_uri(&secret, &user.email, ISSUER)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let qr_svg = qr::render_svg(&uri).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(SetupResponse {
        secret,
        provisioning_uri: uri,
        qr_svg,
    }))
}

/// Verify the user-typed code against the proposed secret, then persist
/// the secret encrypted with KEK and a fresh batch of recovery codes.
#[utoipa::path(
    post,
    path = "/auth/totp/enable",
    tag = "Auth",
    request_body = EnableBody,
    responses(
        (status = 200, description = "2FA enabled; recovery codes shown ONCE", body = EnableResponse),
        (status = 400, description = "Invalid TOTP code"),
        (status = 409, description = "2FA already enabled"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn enable(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Json(body): Json<EnableBody>,
) -> ApiResult<impl IntoResponse> {
    if user.totp_enabled {
        return Err(ApiError::Conflict("2FA already enabled".into()));
    }
    let ok = totp::verify(&body.secret, &body.code)
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    if !ok {
        return Err(ApiError::Validation("invalid 2FA code".into()));
    }

    let encrypted = state
        .kek
        .encrypt(body.secret.as_bytes())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (plaintext_codes, hashed_codes) =
        totp::generate_recovery_codes().map_err(|e| ApiError::Internal(e.to_string()))?;
    users::enable_totp(&state.pool, user.id, &encrypted, &hashed_codes).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.totp_enabled",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    if let Err(e) = session_events::record(
        &state.pool,
        user.id,
        session_events::SessionEvent::TotpEnable,
        crate::routes::auth::client_ip(&headers),
        crate::routes::auth::client_user_agent(&headers).as_deref(),
        json!({}),
    )
    .await
    {
        warn!(?e, user_id = %user.id, "session_events totp_enable record failed");
    }
    info!(user_id = %user.id, "totp enabled");
    Ok(Json(EnableResponse { recovery_codes: plaintext_codes }))
}

/// Disable 2FA. Requires a current valid TOTP code so a stolen session
/// alone can't disable it.
#[utoipa::path(
    post,
    path = "/auth/totp/disable",
    tag = "Auth",
    request_body = VerifyBody,
    responses(
        (status = 200, description = "2FA disabled", body = StatusAck),
        (status = 400, description = "Invalid TOTP code"),
        (status = 409, description = "2FA not enabled"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn disable(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Json(body): Json<VerifyBody>,
) -> ApiResult<impl IntoResponse> {
    if !user.totp_enabled {
        return Err(ApiError::Conflict("2FA not enabled".into()));
    }
    let (secret_encrypted, _) = users::get_totp_material(&state.pool, user.id)
        .await?
        .ok_or(ApiError::Internal("totp material missing".into()))?;
    let secret_bytes = state
        .kek
        .decrypt(&secret_encrypted)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let secret = String::from_utf8(secret_bytes)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let ok = totp::verify(&secret, &body.code)
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    if !ok {
        return Err(ApiError::Validation("invalid 2FA code".into()));
    }
    users::disable_totp(&state.pool, user.id).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.totp_disabled",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    if let Err(e) = session_events::record(
        &state.pool,
        user.id,
        session_events::SessionEvent::TotpDisable,
        crate::routes::auth::client_ip(&headers),
        crate::routes::auth::client_user_agent(&headers).as_deref(),
        json!({}),
    )
    .await
    {
        warn!(?e, user_id = %user.id, "session_events totp_disable record failed");
    }
    info!(user_id = %user.id, "totp disabled");
    Ok(Json(json!({ "status": "ok" })))
}

/// Regenerate recovery codes. Requires a current valid TOTP code (a stolen
/// session alone can't rotate them), invalidates every previous code, and
/// returns the fresh batch exactly once — same contract as `enable`.
#[utoipa::path(
    post,
    path = "/auth/totp/recovery-codes",
    tag = "Auth",
    request_body = VerifyBody,
    responses(
        (status = 200, description = "Fresh recovery codes — shown ONCE; all previous codes are dead", body = EnableResponse),
        (status = 400, description = "Invalid TOTP code"),
        (status = 409, description = "2FA not enabled"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Json(body): Json<VerifyBody>,
) -> ApiResult<impl IntoResponse> {
    if !user.totp_enabled {
        return Err(ApiError::Conflict("2FA not enabled".into()));
    }
    let (secret_encrypted, _) = users::get_totp_material(&state.pool, user.id)
        .await?
        .ok_or(ApiError::Internal("totp material missing".into()))?;
    let secret_bytes = state
        .kek
        .decrypt(&secret_encrypted)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let secret = String::from_utf8(secret_bytes)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let ok = totp::verify(&secret, &body.code)
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    if !ok {
        return Err(ApiError::Validation("invalid 2FA code".into()));
    }

    let (plaintext_codes, hashed_codes) =
        totp::generate_recovery_codes().map_err(|e| ApiError::Internal(e.to_string()))?;
    users::replace_recovery_codes(&state.pool, user.id, &hashed_codes).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.totp_recovery_regenerated",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    if let Err(e) = session_events::record(
        &state.pool,
        user.id,
        session_events::SessionEvent::TotpEnable,
        crate::routes::auth::client_ip(&headers),
        crate::routes::auth::client_user_agent(&headers).as_deref(),
        json!({ "recovery_regenerated": true }),
    )
    .await
    {
        warn!(?e, user_id = %user.id, "session_events recovery_regen record failed");
    }
    info!(user_id = %user.id, "totp recovery codes regenerated");
    Ok(Json(EnableResponse { recovery_codes: plaintext_codes }))
}
