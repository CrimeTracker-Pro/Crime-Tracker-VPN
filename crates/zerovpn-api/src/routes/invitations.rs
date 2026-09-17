//! One-time invitation links. A link proves mailbox access; Google OAuth
//! proves identity and is the only path that activates the account.
use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;
use zerovpn_db::repos::{audit, users};
use zerovpn_mail::templates::VerifyEmail;

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::RequireAdmin,
    state::AppState,
};

const INVITE_TTL: time::Duration = time::Duration::hours(24);

#[derive(Serialize, ToSchema)]
pub struct InvitationAck {
    pub status: &'static str,
}

/// Returned only to an authenticated administrator as a delivery fallback.
/// The plaintext token is never stored in the database.
#[derive(Serialize, ToSchema)]
pub struct InvitationLinkAck {
    pub status: &'static str,
    pub link: String,
}

#[derive(Deserialize, ToSchema)]
pub struct VerifyBody {
    pub token: String,
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn fresh_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plaintext = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash_token(&plaintext);
    (plaintext, hash)
}

#[derive(Serialize, ToSchema)]
pub struct PendingInvitation {
    pub user_id: Uuid,
    pub email: String,
    pub expires_at: OffsetDateTime,
    pub verified_at: Option<OffsetDateTime>,
}

/// Replaces the previous link. No password or working credential is generated.
pub async fn issue(
    state: &AppState,
    user_id: Uuid,
    invited_by: Option<Uuid>,
    email: &str,
) -> ApiResult<()> {
    if !state.mail_limits.check(email, None) {
        return Err(ApiError::RateLimited);
    }
    let (token, hash) = fresh_token();
    let expires_at = OffsetDateTime::now_utc() + INVITE_TTL;
    sqlx::query(
        "INSERT INTO invitations (id, user_id, invited_by, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_id) DO UPDATE SET \
         invited_by = EXCLUDED.invited_by, token_hash = EXCLUDED.token_hash, \
         expires_at = EXCLUDED.expires_at, verified_at = NULL, accepted_at = NULL, revoked_at = NULL",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(invited_by)
    .bind(&hash)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    let link = format!(
        "{}/invite?token={token}",
        state.public_url.trim_end_matches('/')
    );
    if let Some(mailer) = &state.mailer {
        let to: zerovpn_mail::Mailbox = email
            .parse()
            .map_err(|e| ApiError::Internal(format!("invalid recipient: {e}")))?;
        mailer
            .send_email(to, &VerifyEmail { link: &link })
            .await
            .map_err(|e| {
                tracing::error!(?e, %user_id, "invitation email failed");
                ApiError::Internal("invitation email failed".into())
            })?;
    } else {
        tracing::info!(%user_id, link, "DEV: invitation link (no SMTP configured)");
    }
    Ok(())
}

#[utoipa::path(post, path="/auth/invitations/verify", tag="Auth", request_body=VerifyBody,
    responses((status=200, body=InvitationAck), (status=422, description="Invalid, expired or revoked invitation")))]
pub async fn verify(
    State(state): State<AppState>,
    Json(body): Json<VerifyBody>,
) -> ApiResult<impl IntoResponse> {
    if body.token.len() < 16 {
        return Err(ApiError::Validation("invalid invitation".into()));
    }
    let hash = hash_token(&body.token);
    let result = sqlx::query(
        "UPDATE invitations SET verified_at = COALESCE(verified_at, NOW()) WHERE token_hash = $1 \
         AND expires_at > NOW() AND revoked_at IS NULL AND accepted_at IS NULL",
    )
    .bind(hash)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(ApiError::Validation("invalid or expired invitation".into()));
    }
    Ok(Json(InvitationAck { status: "ok" }))
}

#[utoipa::path(get, path="/admin/invitations", tag="Admin", responses((status=200, body=Vec<PendingInvitation>)), security(("session_cookie"=[])))]
pub async fn list(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let rows: Vec<(Uuid, String, OffsetDateTime, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT i.user_id, u.email::TEXT, i.expires_at, i.verified_at FROM invitations i \
         JOIN users u ON u.id = i.user_id WHERE i.accepted_at IS NULL AND i.revoked_at IS NULL \
         AND u.deleted_at IS NULL ORDER BY i.created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(user_id, email, expires_at, verified_at)| PendingInvitation {
                    user_id,
                    email,
                    expires_at,
                    verified_at,
                },
            )
            .collect::<Vec<_>>(),
    ))
}

#[utoipa::path(post, path="/admin/invitations/{id}/resend", tag="Admin", params(("id"=Uuid, Path)), responses((status=200, body=InvitationAck)), security(("session_cookie"=[])))]
pub async fn resend(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let user = users::find_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if user.status != zerovpn_core::models::UserStatus::PendingVerification {
        return Err(ApiError::Validation(
            "invitation is no longer pending".into(),
        ));
    }
    let pending: Option<(Uuid,)> = sqlx::query_as("SELECT user_id FROM invitations WHERE user_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL")
        .bind(id).fetch_optional(&state.pool).await?;
    if pending.is_none() {
        return Err(ApiError::NotFound);
    }
    issue(&state, id, Some(admin.id), &user.email).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(admin.id),
            action: "admin.invitation_resent",
            target_type: Some("user"),
            target_id: Some(id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    Ok(Json(InvitationAck { status: "ok" }))
}

/// Generate a fresh link for a pending invitation without relying on SMTP.
/// This intentionally invalidates the previous link and returns the new one
/// once, so an administrator can deliver it through an alternate channel.
#[utoipa::path(post, path="/admin/invitations/{id}/link", tag="Admin", params(("id"=Uuid, Path)), responses((status=200, body=InvitationLinkAck)), security(("session_cookie"=[])))]
pub async fn create_link(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let user = users::find_by_id(&state.pool, id).await?.ok_or(ApiError::NotFound)?;
    if user.status != zerovpn_core::models::UserStatus::PendingVerification {
        return Err(ApiError::Validation("invitation is no longer pending".into()));
    }
    let (token, hash) = fresh_token();
    let expires_at = OffsetDateTime::now_utc() + INVITE_TTL;
    let updated = sqlx::query(
        "UPDATE invitations SET invited_by = $2, token_hash = $3, expires_at = $4, \
         verified_at = NULL, accepted_at = NULL, revoked_at = NULL \
         WHERE user_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(admin.id)
    .bind(hash)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() != 1 { return Err(ApiError::NotFound); }
    audit::record(&state.pool, audit::AuditEntry {
        actor_user_id: Some(admin.id), action: "admin.invitation_link_created",
        target_type: Some("user"), target_id: Some(id), metadata: json!({}), ip: None,
    }).await?;
    Ok(Json(InvitationLinkAck {
        status: "ok",
        link: format!("{}/invite?token={token}", state.public_url.trim_end_matches('/')),
    }))
}

#[utoipa::path(post, path="/admin/invitations/{id}/revoke", tag="Admin", params(("id"=Uuid, Path)), responses((status=200, body=InvitationAck)), security(("session_cookie"=[])))]
pub async fn revoke(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let result = sqlx::query("UPDATE invitations SET revoked_at = NOW() WHERE user_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL")
        .bind(id).execute(&state.pool).await?;
    if result.rows_affected() != 1 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(admin.id),
            action: "admin.invitation_revoked",
            target_type: Some("user"),
            target_id: Some(id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    Ok(Json(InvitationAck { status: "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_tokens_are_unique_and_stored_as_hashes() {
        let (a, a_hash) = fresh_token();
        let (b, b_hash) = fresh_token();
        assert_ne!(a, b);
        assert_ne!(a_hash, b_hash);
        assert_eq!(a_hash, hash_token(&a));
        assert_eq!(b_hash, hash_token(&b));
        assert_ne!(a, a_hash);
    }
}
