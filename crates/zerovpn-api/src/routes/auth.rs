use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
};
use serde::Serialize;
use serde_json::json;
use std::net::IpAddr;
use tower_sessions::Session;
use tracing::warn;
use utoipa::ToSchema;
use zerovpn_core::models::UserRole;
use zerovpn_db::repos::{session_events, users};

/// Best-effort full client IP from the `X-Forwarded-For` header (the reverse
/// proxy populates this) or the `X-Real-IP` header. Returns `None` if neither
/// is present. Wrapped in `IpNetwork` as a `/32` (v4) or `/128` (v6)
/// host prefix so it lands directly into the `INET`-typed columns
/// (`audit_logs.ip`, `failed_logins.ip`, etc).
///
/// **Phase 2 / Stage A** — the previous implementation truncated to
/// `/24` (v4) or `/48` (v6) to keep the recorded address coarse-grained.
/// That truncation is gone: admins want to see the actual address.
/// **Stage B (migration 20)** completed the schema rename — the columns
/// are now plain `ip` everywhere.
pub(crate) fn client_ip(headers: &HeaderMap) -> Option<ipnetwork::IpNetwork> {
    let raw = headers
        .get("x-forwarded-for")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())?;
    // X-Forwarded-For can be a comma-separated list; the leftmost is the
    // client, the rest are proxies. We only trust the leftmost — anything
    // beyond that is hop-by-hop and can be spoofed by intermediate hops.
    let first = raw.split(',').next()?.trim();
    let ip: IpAddr = first.parse().ok()?;
    Some(ipnetwork::IpNetwork::from(ip))
}

/// Raw `User-Agent` header, lowercased and trimmed. Returned for storage
/// in `failed_logins.user_agent` / `sessions.user_agent` — admins use it
/// to spot brute-force tooling (`curl/...`, `python-requests/...`) and
/// to correlate suspicious-login emails with the browser the user was
/// actually on. Stored in plaintext, no longer hashed.
pub(crate) fn client_user_agent(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(axum::http::header::USER_AGENT)?.to_str().ok()?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::{CurrentUser, SESSION_KEY_REAL_USER_ID, SESSION_KEY_USER_ID},
    routes::dto::StatusAck,
    state::AppState,
};

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub user: PublicUser,
    pub must_change_password: bool,
    pub totp_required: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PublicUser {
    pub id: uuid::Uuid,
    pub email: String,
    pub role: UserRole,
    /// True when the user has finished TOTP enrollment. Surfaced here
    /// so the frontend can auto-detect 2FA status on the Security page
    /// instead of guessing.
    pub totp_enabled: bool,
    /// True when the current session is an admin impersonating this account.
    #[serde(default)]
    pub is_impersonated: bool,
    /// Email of the admin who initiated impersonation. Only present when
    /// `is_impersonated` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impersonator_email: Option<String>,
    /// Admin-set global toggles that govern what *non-admin* users see in
    /// the user-facing app. Always present on `/me`, the login response,
    /// and the email-verify response so the SPA can gate routes/links from
    /// first paint without an extra round-trip. Admins should ignore these
    /// — backend handlers exempt admins from policy gating.
    pub user_policy: UserPolicySnapshot,
}

#[derive(Debug, Serialize, ToSchema, Default, sqlx::FromRow)]
pub struct UserPolicySnapshot {
    pub hide_device_detail: bool,
}

/// Fetch the current global user policy. Mirrors the shape returned by
/// `GET /admin/user-policy` but is used to enrich every PublicUser
/// response so the frontend learns the policy on login / page-load
/// without a separate request. Falls back to permissive defaults on
/// query failure so a transient DB hiccup never locks users out.
pub async fn load_user_policy(pool: &zerovpn_db::PgPool) -> UserPolicySnapshot {
    sqlx::query_as::<_, UserPolicySnapshot>(
        "SELECT policy_hide_device_detail AS hide_device_detail
           FROM app_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap_or_default()
}


/// Verify a code against the stored TOTP secret OR consume a recovery code.
/// On a successful recovery match, that code is removed from the user's set.
pub(crate) async fn verify_totp_or_recovery(
    state: &AppState,
    user_id: uuid::Uuid,
    code: &str,
) -> ApiResult<bool> {
    let (secret_encrypted, recovery_hashes) =
        match users::get_totp_material(&state.pool, user_id).await? {
            Some(t) => t,
            None => return Ok(false),
        };
    let secret_bytes = state
        .kek
        .decrypt(&secret_encrypted)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let secret = String::from_utf8(secret_bytes)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if let Ok(true) = zerovpn_auth::totp::verify(&secret, code) {
        return Ok(true);
    }
    if let Ok(Some(idx)) = zerovpn_auth::totp::match_recovery_code(code, &recovery_hashes) {
        let mut remaining = recovery_hashes.clone();
        remaining.remove(idx);
        users::replace_recovery_codes(&state.pool, user_id, &remaining).await?;
        return Ok(true);
    }
    Ok(false)
}

#[utoipa::path(
    post,
    path = "/auth/logout",
    tag = "Auth",
    responses(
        (status = 200, description = "Session flushed", body = StatusAck),
    ),
    security(("session_cookie" = [])),
)]
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // Read the user_id off the session *before* flush so we can attribute
    // the session_events row. Missing key (already logged out / never
    // authed) is fine — we just skip the record.
    let user_id: Option<uuid::Uuid> = session
        .get(SESSION_KEY_USER_ID)
        .await
        .ok()
        .flatten();
    session
        .flush()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if let Some(uid) = user_id
        && let Err(e) = session_events::record(
            &state.pool,
            uid,
            session_events::SessionEvent::Logout,
            client_ip(&headers),
            client_user_agent(&headers).as_deref(),
            json!({}),
        )
        .await
        {
            warn!(?e, user_id = %uid, "session_events logout record failed");
        }
    Ok(Json(json!({ "status": "ok" })))
}

#[utoipa::path(
    get,
    path = "/me",
    tag = "Account",
    responses(
        (status = 200, description = "Authenticated user", body = PublicUser),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn me(
    State(state): State<AppState>,
    session: Session,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let real_user_id: Option<uuid::Uuid> = session
        .get(SESSION_KEY_REAL_USER_ID)
        .await
        .unwrap_or(None);

    let impersonator_email = if let Some(rid) = real_user_id {
        users::find_by_id(&state.pool, rid)
            .await?
            .map(|u| u.email)
    } else {
        None
    };

    let user_policy = load_user_policy(&state.pool).await;
    Ok(Json(PublicUser {
        id: user.id,
        email: user.email,
        role: user.role,
        totp_enabled: user.totp_enabled,
        is_impersonated: impersonator_email.is_some(),
        impersonator_email,
        user_policy,
    }))
}
