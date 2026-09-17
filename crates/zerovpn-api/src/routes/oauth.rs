//! Google OAuth 2.0 sign-in.
//!
//! Two-step flow shaped to keep the user's browser doing the navigation
//! (Google won't show the consent page inside an XHR):
//!
//!   1. `GET /auth/google/start` — top-level navigation from the SPA's
//!      "Continue with Google" button. Mints a random `state` + PKCE
//!      verifier, parks the (hashed) state and verifier in
//!      `oauth_states`, and 302s the browser to Google's auth URL.
//!
//!   2. After Google bounces the browser back to the operator-registered
//!      redirect URL (a SPA route under the frontend dev server),
//!      `POST /auth/google/callback` is called by the SPA with the
//!      `{code, state}` query params it just received. The server:
//!      - consumes the state row (one-shot — replay can't reuse it),
//!      - exchanges the code at Google's token endpoint using the stored
//!        PKCE verifier,
//!      - fetches userinfo, refuses if `email_verified == false`,
//!      - looks up the user by google_id, then by email (auto-link). Unknown
//!        identities are denied: accounts are created only by an admin invite
//!        or the deployment bootstrap-admin setting.
//!      - mints a session and returns the
//!        `LoginResponse` shape so the SPA's auth store hydrates the
//!        same way.
//!
//! TOTP is intentionally NOT re-prompted: the Google sign-in is treated as
//! a complete authentication on its own (standard SSO behavior). A user
//! who wants TOTP-required-everywhere should disable the Google link.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};
use url::Url;
use utoipa::ToSchema;
use zerovpn_core::models::UserStatus;
use zerovpn_db::repos::{audit, failed_logins, oauth_states, session_events, users};

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::{
        SESSION_KEY_PENDING_TOTP_USER, SESSION_KEY_PW_CHANGED_AT, SESSION_KEY_USER_ID,
    },
    routes::auth::{
        LoginResponse, PublicUser, client_ip, client_user_agent, load_user_policy,
        verify_totp_or_recovery,
    },
    state::AppState,
};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";

/// Lifetime of an OAuth `state` row. The round-trip through Google should
/// take seconds; ten minutes is generous and keeps the table small.
const STATE_TTL: Duration = Duration::minutes(10);

#[utoipa::path(
    get,
    path = "/auth/google/start",
    tag = "Auth",
    responses(
        (status = 302, description = "Redirect to Google's consent screen"),
        (status = 503, description = "Google OAuth not configured on this deployment"),
    ),
)]
pub async fn google_start(State(state): State<AppState>) -> ApiResult<Response> {
    let cfg = match &state.google_oauth {
        Some(c) => c.clone(),
        None => return Ok(google_disabled()),
    };

    let state_param = random_url_safe(32);
    let verifier = random_url_safe(48);
    let challenge = {
        let mut h = Sha256::new();
        h.update(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(h.finalize())
    };
    let state_hash = sha256_hex(&state_param);

    oauth_states::insert(
        &state.pool,
        &state_hash,
        &verifier,
        OffsetDateTime::now_utc() + STATE_TTL,
    )
    .await?;

    // url::Url::query_pairs_mut handles percent-encoding for us so values
    // with `+` / `=` / space don't break the URL.
    let mut url = Url::parse(GOOGLE_AUTH_URL).map_err(|e| ApiError::Internal(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", &cfg.redirect_url)
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", &state_param)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        // `select_account` makes Google show the chooser even when the
        // user is signed into exactly one Google account — useful for
        // "switch account" flows from the SPA.
        .append_pair("prompt", "select_account");

    Ok(Redirect::to(url.as_str()).into_response())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GoogleCallbackBody {
    /// The `code` Google appended to the redirect URL.
    pub code: String,
    /// The `state` we parked at `/auth/google/start`; matched against the
    /// hashed copy in `oauth_states`.
    pub state: String,
}

#[utoipa::path(
    post,
    path = "/auth/google/callback",
    tag = "Auth",
    request_body = GoogleCallbackBody,
    responses(
        (status = 200, description = "Session established", body = LoginResponse),
        (status = 400, description = "Bad state, expired state, or unverified Google email"),
        (status = 403, description = "Account suspended"),
        (status = 502, description = "Token exchange or userinfo call failed at Google"),
        (status = 503, description = "Google OAuth not configured on this deployment"),
    ),
)]
pub async fn google_callback(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    headers: HeaderMap,
    Json(body): Json<GoogleCallbackBody>,
) -> ApiResult<impl IntoResponse> {
    let cfg = match &state.google_oauth {
        Some(c) => c.clone(),
        None => {
            return Err(ApiError::Internal(
                "google oauth not configured".to_string(),
            ));
        }
    };

    let state_hash = sha256_hex(&body.state);
    let verifier = oauth_states::consume(&state.pool, &state_hash)
        .await?
        .ok_or_else(|| ApiError::Validation("invalid or expired state".to_string()))?;

    let token = exchange_code(&cfg, &body.code, &verifier).await?;
    let info = fetch_userinfo(&token.access_token).await?;

    if !info.email_verified {
        return Err(ApiError::Validation(
            "google account email is not verified".to_string(),
        ));
    }
    let email = info.email.trim().to_lowercase();
    let google_id = info.sub;
    let req_ip = client_ip(&headers);
    let req_ua = client_user_agent(&headers);

    // 1) Already linked? Sign in directly.
    let user = if let Some(u) = users::find_by_google_id(&state.pool, &google_id).await? {
        u
    }
    // 2) Existing active account can link its verified Google identity.
    else if let Some(u) = users::find_by_email(&state.pool, &email).await? {
        if u.status == UserStatus::PendingVerification {
            // Consume the verified invitation and activate this exact email
            // atomically. A direct Google login cannot bypass the mail link.
            let mut tx = state.pool.begin().await?;
            let consumed: Option<(uuid::Uuid,)> = sqlx::query_as(
                "UPDATE invitations SET accepted_at = NOW() WHERE user_id = $1 \
                 AND verified_at IS NOT NULL AND expires_at > NOW() \
                 AND revoked_at IS NULL AND accepted_at IS NULL RETURNING user_id",
            ).bind(u.id).fetch_optional(&mut *tx).await?;
            if consumed.is_none() { return Err(ApiError::EmailNotVerified); }
            let updated = sqlx::query(
                "UPDATE users SET status = 'active', google_id = $2, \
                 email_verified_at = NOW(), must_change_password = FALSE \
                 WHERE id = $1 AND status = 'pending_verification' AND google_id IS NULL",
            ).bind(u.id).bind(&google_id).execute(&mut *tx).await?;
            if updated.rows_affected() != 1 { return Err(ApiError::Forbidden); }
            tx.commit().await?;
            let _ = audit::record_with_ua(&state.pool, audit::AuditEntry {
                actor_user_id: Some(u.id), action: "auth.invitation_accepted",
                target_type: Some("user"), target_id: Some(u.id),
                metadata: json!({ "via": "google" }), ip: req_ip,
            }, req_ua.as_deref()).await;
        } else {
            if u.status != UserStatus::Active { return Err(ApiError::Forbidden); }
            if users::link_google(&state.pool, u.id, &google_id).await? != 1 {
                return Err(ApiError::Forbidden);
            }
        }
        info!(user_id = %u.id, "linked existing account to google");
        let _ = audit::record_with_ua(
            &state.pool,
            audit::AuditEntry {
                actor_user_id: Some(u.id),
                action: "auth.google.linked",
                target_type: Some("user"),
                target_id: Some(u.id),
                metadata: json!({ "google_sub": google_id }),
                ip: req_ip,
            },
            req_ua.as_deref(),
        )
        .await;
        // Re-fetch so the secrets row reflects the linked state (we want
        // the updated email_verified_at, etc).
        users::find_by_email(&state.pool, &email)
            .await?
            .ok_or_else(|| ApiError::Internal("user vanished after link".to_string()))?
    }
    // 3) Brand-new Google identities must never self-provision. The account
    //    must have been created by an admin invitation (or bootstrap) first.
    else {
        warn!(email, "rejected Google login for uninvited identity");
        return Err(ApiError::Forbidden);
    };

    if user.status != UserStatus::Active && user.status != UserStatus::PendingVerification {
        return Err(ApiError::Forbidden);
    }
    if user.status == UserStatus::PendingVerification {
        // An invitation is not usable until its one-time email link is
        // consumed. This also prevents an invitee from skipping the mailbox
        // verification step by going directly to Google.
        return Err(ApiError::EmailNotVerified);
    }

    let user_policy = load_user_policy(&state.pool).await;

    // 2FA gate. Google sign-in verifies the *Google* identity, but if the
    // Crime Tracker VPN account has its own TOTP enabled we still require it — the
    // Google path must not be a way to skip 2FA. Hold a half-authenticated
    // "pending TOTP" session (NOT the real one) and make the client finish
    // the challenge via `/auth/google/verify-totp`.
    if user.totp_enabled {
        session
            .insert(SESSION_KEY_PENDING_TOTP_USER, user.id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        info!(user_id = %user.id, "google login: awaiting 2fa");
        return Ok(Json(LoginResponse {
            user: PublicUser {
                id: user.id,
                email: user.email,
                role: user.role,
                totp_enabled: true,
                is_impersonated: false,
                impersonator_email: None,
                user_policy,
            },
            must_change_password: user.must_change_password,
            totp_required: true,
        }));
    }

    // No 2FA on the account — mint the real session directly.
    session
        .insert(SESSION_KEY_USER_ID, user.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(
            SESSION_KEY_PW_CHANGED_AT,
            user.password_changed_at.unix_timestamp(),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    users::touch_last_login(&state.pool, user.id).await?;

    if let Err(e) = session_events::record(
        &state.pool,
        user.id,
        session_events::SessionEvent::Login,
        req_ip,
        req_ua.as_deref(),
        json!({ "via": "google" }),
    )
    .await
    {
        warn!(?e, user_id = %user.id, "session_events google login record failed");
    }

    info!(user_id = %user.id, role = ?user.role, "google login");

    Ok(Json(LoginResponse {
        user: PublicUser {
            id: user.id,
            email: user.email,
            role: user.role,
            totp_enabled: false,
            is_impersonated: false,
            impersonator_email: None,
            user_policy,
        },
        must_change_password: user.must_change_password,
        totp_required: false,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct GoogleTotpBody {
    pub totp_code: String,
}

/// Second leg of a Google sign-in for a 2FA-enabled account. The callback
/// left a `pending_totp` marker in the session (no real session yet); this
/// verifies the supplied TOTP or recovery code and, on success, upgrades the
/// session to a fully-authenticated one. Same `LoginResponse` shape as the
/// callback so the SPA hydrates identically.
#[utoipa::path(
    post,
    path = "/auth/google/verify-totp",
    tag = "Auth",
    responses(
        (status = 200, description = "2FA verified; session established", body = LoginResponse),
        (status = 401, description = "No pending Google 2FA challenge, or wrong code"),
        (status = 403, description = "Account suspended"),
    ),
)]
pub async fn google_verify_totp(
    State(state): State<AppState>,
    session: tower_sessions::Session,
    headers: HeaderMap,
    Json(body): Json<GoogleTotpBody>,
) -> ApiResult<impl IntoResponse> {
    // A request without the pending marker has no challenge to answer — treat
    // as unauthorized rather than leaking whether a challenge exists.
    let user_id: uuid::Uuid = session
        .get(SESSION_KEY_PENDING_TOTP_USER)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;

    let req_ip = client_ip(&headers);
    let req_ua = client_user_agent(&headers);

    let code = body.totp_code.trim();
    let ok = if code.is_empty() {
        false
    } else {
        verify_totp_or_recovery(&state, user_id, code).await?
    };
    if !ok {
        let email = users::find_by_id(&state.pool, user_id)
            .await?
            .map(|u| u.email);
        let _ = failed_logins::record(
            &state.pool,
            email.as_deref(),
            req_ip,
            req_ua.as_deref(),
            failed_logins::FailedLoginReason::TotpFailed,
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    // Re-load + re-check status, then swap the pending marker for a real
    // session (drop the pending key so it can't be replayed).
    let user = users::find_by_id(&state.pool, user_id)
        .await?
        .ok_or_else(|| ApiError::Internal("pending user vanished".to_string()))?;
    if user.status != UserStatus::Active {
        return Err(ApiError::Forbidden);
    }

    session
        .remove::<uuid::Uuid>(SESSION_KEY_PENDING_TOTP_USER)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(SESSION_KEY_USER_ID, user.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(
            SESSION_KEY_PW_CHANGED_AT,
            user.password_changed_at.unix_timestamp(),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    users::touch_last_login(&state.pool, user.id).await?;
    if let Err(e) = session_events::record(
        &state.pool,
        user.id,
        session_events::SessionEvent::Login,
        req_ip,
        req_ua.as_deref(),
        json!({ "via": "google+totp" }),
    )
    .await
    {
        warn!(?e, user_id = %user.id, "session_events google+totp login record failed");
    }
    info!(user_id = %user.id, role = ?user.role, "google login (2fa verified)");

    let user_policy = load_user_policy(&state.pool).await;
    Ok(Json(LoginResponse {
        user: PublicUser {
            id: user.id,
            email: user.email,
            role: user.role,
            totp_enabled: user.totp_enabled,
            is_impersonated: false,
            impersonator_email: None,
            user_policy,
        },
        must_change_password: user.must_change_password,
        totp_required: false,
    }))
}

// ---------------------------------------------------------------------------
// HTTP calls to Google
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    #[allow(dead_code)]
    id_token: Option<String>,
}

async fn exchange_code(
    cfg: &zerovpn_core::config::GoogleOAuthConfig,
    code: &str,
    verifier: &str,
) -> ApiResult<TokenResponse> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::Internal(format!("http client: {e}")))?;

    // Hand-rolled form encoding — reqwest's `.form()` is feature-gated and
    // this workspace doesn't pull it in. `url::form_urlencoded` is already
    // in our dep graph and gives us deterministic `application/x-www-form-urlencoded`.
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("code", code)
        .append_pair("client_id", &cfg.client_id)
        .append_pair("client_secret", &cfg.client_secret)
        .append_pair("redirect_uri", &cfg.redirect_url)
        .append_pair("grant_type", "authorization_code")
        .append_pair("code_verifier", verifier)
        .finish();

    let res = client
        .post(GOOGLE_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("token exchange: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        warn!(%status, %body, "google token exchange failed");
        return Err(ApiError::Internal(format!(
            "google token exchange returned {status}"
        )));
    }
    res.json::<TokenResponse>()
        .await
        .map_err(|e| ApiError::Internal(format!("token response decode: {e}")))
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    sub: String,
    email: String,
    #[serde(default)]
    email_verified: bool,
}

async fn fetch_userinfo(access_token: &str) -> ApiResult<UserInfo> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::Internal(format!("http client: {e}")))?;

    let res = client
        .get(GOOGLE_USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("userinfo: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        warn!(%status, %body, "google userinfo fetch failed");
        return Err(ApiError::Internal(format!(
            "google userinfo returned {status}"
        )));
    }
    res.json::<UserInfo>()
        .await
        .map_err(|e| ApiError::Internal(format!("userinfo decode: {e}")))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn random_url_safe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

fn google_disabled() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "code": "google_oauth_disabled",
                "message": "google oauth is not configured on this deployment",
            }
        })),
    )
        .into_response()
}
