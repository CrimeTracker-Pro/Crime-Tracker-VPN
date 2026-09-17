//! Email verification + password reset routes.
//!
//! Both flows share the same token table (`verification_tokens`) and the
//! same email-by-link UX. We hash the plaintext token (sha256) at rest so
//! a stolen DB doesn't directly hand over working links.

use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use garde::Validate;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use utoipa::ToSchema;
use zerovpn_core::models::UserStatus;
use zerovpn_db::repos::{audit, session_events, users, verification_tokens};
use zerovpn_db::repos::verification_tokens::TokenPurpose;
use zerovpn_mail::templates::{PasswordReset, VerifyEmail};

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

const VERIFY_TOKEN_TTL: time::Duration = time::Duration::hours(24);
const RESET_TOKEN_TTL: time::Duration = time::Duration::hours(1);

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct ForgotBody {
    #[garde(email)]
    pub email: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct VerifyBody {
    #[garde(length(min = 16))]
    pub token: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct ResetBody {
    #[garde(length(min = 16))]
    pub token: String,
    #[garde(length(min = 12, max = 128))]
    pub new_password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Ack {
    #[schema(example = "ok")]
    pub status: &'static str,
}

/// Generate a 32-byte URL-safe token + return (plaintext, sha256_hex).
pub fn fresh_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plaintext = URL_SAFE_NO_PAD.encode(bytes);
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    let hash = hex::encode(hasher.finalize());
    (plaintext, hash)
}

pub fn hash_token(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

/// Create a new email-verify token + send the link. Idempotent: invalidates
/// any active verify tokens for the user before issuing a new one.
pub async fn issue_verify_email(
    state: &AppState,
    user_id: uuid::Uuid,
    email: &str,
) -> ApiResult<()> {
    verification_tokens::invalidate_active(&state.pool, user_id, TokenPurpose::EmailVerify).await?;
    let (plaintext, hash) = fresh_token();
    verification_tokens::create(
        &state.pool,
        user_id,
        TokenPurpose::EmailVerify,
        &hash,
        VERIFY_TOKEN_TTL,
    )
    .await?;

    let link = format!(
        "{}/verify-email?token={}",
        state.public_url.trim_end_matches('/'),
        plaintext
    );

    if let Some(mailer) = &state.mailer {
        let to: zerovpn_mail::Mailbox = email
            .parse()
            .map_err(|e| ApiError::Internal(format!("invalid to: {e}")))?;
        if let Err(e) = mailer.send_email(to, &VerifyEmail { link: &link }).await {
            warn!(?e, "verify email send failed");
        }
    } else {
        info!(%user_id, link, "DEV: verify-email link (no SMTP configured)");
    }
    Ok(())
}

/// Mint a fresh password-reset token for the user, invalidate any earlier
/// active reset tokens, and ship the link via SMTP (or log it in dev when
/// no mailer is configured). Shared by the public forgot-password flow and
/// the admin "send reset link" override on the user-detail page.
pub async fn issue_password_reset(
    state: &AppState,
    user_id: uuid::Uuid,
    email: &str,
) -> ApiResult<()> {
    verification_tokens::invalidate_active(&state.pool, user_id, TokenPurpose::PasswordReset)
        .await?;
    let (plaintext, hash) = fresh_token();
    verification_tokens::create(
        &state.pool,
        user_id,
        TokenPurpose::PasswordReset,
        &hash,
        RESET_TOKEN_TTL,
    )
    .await?;

    let link = format!(
        "{}/reset-password?token={}",
        state.public_url.trim_end_matches('/'),
        plaintext
    );

    if let Some(mailer) = &state.mailer {
        let to: zerovpn_mail::Mailbox = email
            .parse()
            .map_err(|e| ApiError::Internal(format!("invalid to: {e}")))?;
        if let Err(e) = mailer
            .send_email(to, &PasswordReset { link: &link })
            .await
        {
            warn!(?e, "password reset email send failed");
        }
    } else {
        info!(%user_id, link, "DEV: password-reset link (no SMTP configured)");
    }
    Ok(())
}

#[derive(Debug, Serialize, ToSchema)]
#[derive(Debug, Serialize, ToSchema)]
pub struct VerifyEmailResponse {
    #[schema(example = "ok")]
    pub status: &'static str,
}

#[utoipa::path(
    post,
    path = "/auth/verify-email",
    tag = "Auth",
    request_body = VerifyBody,
    responses(
        (status = 200, description = "Email verified; session established", body = VerifyEmailResponse),
        (status = 400, description = "Invalid / expired / wrong-purpose token"),
    ),
)]
pub async fn verify_email(
    State(state): State<AppState>,
    Json(body): Json<VerifyBody>,
) -> ApiResult<impl IntoResponse> {
    body.validate().map_err(|e| ApiError::Validation(e.to_string()))?;
    let hash = hash_token(&body.token);
    let token = verification_tokens::find_active(&state.pool, &hash)
        .await?
        .ok_or_else(|| ApiError::Validation("invalid or expired token".into()))?;
    if token.purpose != TokenPurpose::EmailVerify {
        return Err(ApiError::Validation("wrong token purpose".into()));
    }
    sqlx::query(
        r#"UPDATE users
              SET status = 'active',
                  email_verified_at = COALESCE(email_verified_at, NOW())
            WHERE id = $1 AND deleted_at IS NULL"#,
    )
    .bind(token.user_id)
    .execute(&state.pool)
    .await?;
    verification_tokens::consume(&state.pool, token.id).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(token.user_id),
            action: "user.email_verified",
            target_type: Some("user"),
            target_id: Some(token.user_id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;

    // Verification proves control of the invitation mailbox; identity and
    // session establishment are deliberately deferred to Google OAuth.
    info!(user_id = %token.user_id, "invitation email verified; awaiting Google sign-in");
    Ok(Json(VerifyEmailResponse { status: "ok" }))
}

#[utoipa::path(
    post,
    path = "/auth/forgot-password",
    tag = "Auth",
    request_body = ForgotBody,
    responses(
        (status = 200, description = "Acknowledged (enumeration-safe — same response whether the email exists or not)", body = Ack),
    ),
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ForgotBody>,
) -> ApiResult<impl IntoResponse> {
    body.validate().map_err(|e| ApiError::Validation(e.to_string()))?;
    let email = body.email.trim().to_lowercase();
    // Reset links go to *existing* addresses, so without a ceiling this
    // endpoint can mail-bomb a known user (and burn the relay's quota).
    if !state
        .mail_limits
        .check(&email, crate::routes::auth::client_ip(&headers))
    {
        return Err(ApiError::RateLimited);
    }
    let user = users::find_by_email(&state.pool, &email).await?;

    // Email-enumeration prevention: always return 200 with the same
    // shape regardless of whether the address exists. The email is only
    // sent (and the audit row only written) when the user is real and
    // in a state where receiving a reset link makes sense.
    if let Some(u) = user
        && (u.status == UserStatus::Active || u.status == UserStatus::PendingVerification) {
            issue_password_reset(&state, u.id, &u.email).await?;
            // Audit the request itself (not just the eventual reset).
            // Useful when looking at "did someone try to take over this
            // account?" after the fact.
            audit::record(
                &state.pool,
                audit::AuditEntry {
                    actor_user_id: Some(u.id),
                    action: "user.password_reset_requested",
                    target_type: Some("user"),
                    target_id: Some(u.id),
                    metadata: json!({}),
                    ip: None,
                },
            )
            .await?;
        }
    Ok(Json(Ack { status: "ok" }))
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct VerifyResetTokenBody {
    #[garde(length(min = 16))]
    pub token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VerifyResetTokenResponse {
    pub valid: bool,
    /// Reason `valid` is false. Omitted on success. Stable enum-shaped
    /// values so the frontend can switch on them. One of
    /// `"invalid" | "used" | "wrong_purpose" | "expired"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// Pre-flight check for a reset-password link. Lets the frontend show an
/// "expired link" state up front instead of letting the user type a new
/// password and then surfacing the failure at submit-time.
///
/// We deliberately look up by hash WITHOUT the active-window filter so we
/// can tell the user *why* a link doesn't work — "used" (likely a stale
/// link from before they requested a newer one), "expired" (actually past
/// TTL), or "invalid" (hash not in DB at all, e.g. mangled URL or token
/// from a DB that's since been reset). Reporting all three as "expired"
/// made it impossible to distinguish a real expiry from "you clicked the
/// older of two emails", which is a very easy mistake to make.
#[utoipa::path(
    post,
    path = "/auth/verify-reset-token",
    tag = "Auth",
    request_body = VerifyResetTokenBody,
    responses(
        (status = 200, description = "Pre-flight check result", body = VerifyResetTokenResponse),
    ),
)]
pub async fn verify_reset_token(
    State(state): State<AppState>,
    Json(body): Json<VerifyResetTokenBody>,
) -> ApiResult<impl IntoResponse> {
    if body.validate().is_err() {
        info!("verify_reset_token: malformed token in request body");
        return Ok(Json(VerifyResetTokenResponse {
            valid: false,
            reason: Some("invalid"),
        }));
    }
    let hash = hash_token(&body.token);
    let row = verification_tokens::find_by_hash(&state.pool, &hash).await?;
    let now = time::OffsetDateTime::now_utc();
    let (valid, reason) = match row {
        None => (false, Some("invalid")),
        Some(t) if t.purpose != TokenPurpose::PasswordReset => (false, Some("wrong_purpose")),
        Some(t) if t.consumed_at.is_some() => (false, Some("used")),
        Some(t) if t.expires_at <= now => (false, Some("expired")),
        Some(_) => (true, None),
    };
    if !valid {
        info!(
            reason = ?reason,
            "verify_reset_token: rejecting link",
        );
    }
    Ok(Json(VerifyResetTokenResponse { valid, reason }))
}

#[utoipa::path(
    post,
    path = "/auth/reset-password",
    tag = "Auth",
    request_body = ResetBody,
    responses(
        (status = 200, description = "Password updated; every existing session for the user is now dead", body = Ack),
        (status = 400, description = "Invalid / expired / wrong-purpose token"),
    ),
)]
pub async fn reset_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResetBody>,
) -> ApiResult<impl IntoResponse> {
    body.validate().map_err(|e| ApiError::Validation(e.to_string()))?;
    let hash = hash_token(&body.token);
    let token = verification_tokens::find_active(&state.pool, &hash)
        .await?
        .ok_or_else(|| ApiError::Validation("invalid or expired token".into()))?;
    if token.purpose != TokenPurpose::PasswordReset {
        return Err(ApiError::Validation("wrong token purpose".into()));
    }
    let pw_hash = zerovpn_auth::password::hash(&body.new_password)?;
    // Bump password_changed_at in the same UPDATE. The auth extractor
    // diffs the live column against the snapshot stored in each session,
    // so this single statement invalidates every outstanding session for
    // the user — including the one that may have requested the reset.
    users::update_password(&state.pool, token.user_id, &pw_hash).await?;
    verification_tokens::consume(&state.pool, token.id).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(token.user_id),
            action: "user.password_reset",
            target_type: Some("user"),
            target_id: Some(token.user_id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    // Phase 2 / Stage B — session_events row, distinguished from
    // settings-driven changes via metadata.via.
    if let Err(e) = session_events::record(
        &state.pool,
        token.user_id,
        session_events::SessionEvent::PasswordChange,
        crate::routes::auth::client_ip(&headers),
        crate::routes::auth::client_user_agent(&headers).as_deref(),
        json!({ "via": "email_link" }),
    )
    .await
    {
        warn!(?e, user_id = %token.user_id, "session_events password_reset record failed");
    }
    info!(user_id = %token.user_id, "password reset");
    Ok(Json(Ack { status: "ok" }))
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct ResendBody {
    #[garde(email)]
    pub email: String,
}

/// How many verify emails a single pending account may trigger in the
/// window below. Hitting the cap silently no-ops (no enumeration leak,
/// no SMTP hammer) — the user still has an active link in their inbox.
const RESEND_CAP: i64 = 3;
const RESEND_WINDOW_SECS: i64 = 10 * 60;

#[utoipa::path(
    post,
    path = "/auth/resend-verify",
    tag = "Auth",
    request_body = ResendBody,
    responses(
        (status = 200, description = "Acknowledged (enumeration-safe; rate-limited per address)", body = Ack),
    ),
)]
pub async fn resend_verify(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ResendBody>,
) -> ApiResult<impl IntoResponse> {
    body.validate().map_err(|e| ApiError::Validation(e.to_string()))?;
    let email = body.email.trim().to_lowercase();
    // Layered on top of the per-user token cap below: this also covers
    // addresses with no account and adds the per-IP ceiling.
    if !state
        .mail_limits
        .check(&email, crate::routes::auth::client_ip(&headers))
    {
        return Err(ApiError::RateLimited);
    }
    if let Some(u) = users::find_by_email(&state.pool, &email).await?
        && u.status == UserStatus::PendingVerification {
            let recent = verification_tokens::count_recent_for_user(
                &state.pool,
                u.id,
                TokenPurpose::EmailVerify,
                RESEND_WINDOW_SECS,
            )
            .await?;
            if recent < RESEND_CAP {
                issue_verify_email(&state, u.id, &u.email).await?;
            } else {
                info!(
                    user_id = %u.id,
                    recent,
                    cap = RESEND_CAP,
                    "resend-verify throttled"
                );
            }
        }
    Ok(Json(Ack { status: "ok" }))
}
