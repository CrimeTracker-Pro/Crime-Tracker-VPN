use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use tower_sessions::Session;
use tracing::info;
use utoipa::{IntoParams, ToSchema};
use zerovpn_db::repos::{
    audit, devices, servers, topology_positions, user_prefs, user_sessions, users,
};

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::{CurrentUser, SESSION_KEY_PW_CHANGED_AT},
    routes::dto::StatusAck,
    state::AppState,
};

#[derive(Debug, Serialize, ToSchema)]
pub struct DataExport {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// The authenticated user's `User` row (no password / TOTP material).
    pub user: serde_json::Value,
    /// Every device owned by the user, regardless of status.
    pub devices: serde_json::Value,
    /// Audit entries this user authored. Admin-target rows on this user
    /// are deliberately excluded from this scope.
    pub audit: serde_json::Value,
}

/// GDPR data export: returns a JSON blob with everything we have on the
/// authenticated user, excluding password hashes / TOTP secrets.
#[utoipa::path(
    get,
    path = "/me/data-export",
    tag = "Account",
    responses(
        (status = 200, description = "User-scoped data export", body = DataExport),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn export(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    // User row (already public-safe via the User type)
    let user_json = serde_json::to_value(&user)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // All devices owned by user (regardless of status)
    let devs = sqlx::query_as::<_, zerovpn_core::models::Device>(
        r#"SELECT id, user_id, server_id, name, os, device_type, public_key, allocated_ip,
                  status, allowed_ips_override,
                  last_handshake_at, created_at,
                  NULL::bytea AS private_key_encrypted
             FROM devices
            WHERE user_id = $1
            ORDER BY created_at DESC"#,
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let devices_json = serde_json::to_value(&devs)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Audit entries the user is the actor of (admins also have entries on
    // them as targets, but we keep this scoped to actor for the user's
    // own export).
    #[derive(serde::Serialize, sqlx::FromRow)]
    struct AuditEntry {
        action: String,
        target_type: Option<String>,
        target_id: Option<uuid::Uuid>,
        metadata: serde_json::Value,
        created_at: OffsetDateTime,
    }
    let entries: Vec<AuditEntry> = sqlx::query_as(
        r#"SELECT action, target_type, target_id, metadata, created_at
             FROM audit_logs
            WHERE actor_user_id = $1
            ORDER BY id DESC"#,
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let audit_json = serde_json::to_value(&entries)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let device_count = devs.len();
    let entry_count = entries.len();
    drop(devs);
    drop(entries);
    info!(user_id = %user.id, devices = device_count, audit = entry_count, "data export");
    Ok(Json(DataExport {
        generated_at: OffsetDateTime::now_utc(),
        user: user_json,
        devices: devices_json,
        audit: audit_json,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MyServerInfo {
    /// CIDR of the WG subnet (e.g. "10.10.0.0/22"). Used by the create-
    /// device dialog to render "must be inside <cidr>" hints and to
    /// pre-fill the split-tunnel allowed_ips.
    pub cidr: String,
    /// Public hostname:port the user's clients dial. Echoed back so
    /// the create dialog can show "you'll connect via <host>".
    pub endpoint_host: String,
    pub endpoint_port: i32,
    pub mtu: i32,
}

/// Public-safe info about the user's WG server. Exposes the same values
/// that already end up in every `.conf` file the user downloads — no
/// secrets (private keys, internal kek, etc) are included. Anyone with
/// a valid session can call this.
#[utoipa::path(
    get,
    path = "/me/server",
    tag = "Account",
    responses(
        (status = 200, description = "Public WG server defaults for the create-device dialog", body = MyServerInfo),
        (status = 404, description = "No active server yet"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn server_info(
    State(state): State<AppState>,
    CurrentUser(_user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let active = servers::list_active(&state.pool).await?;
    let s = active.into_iter().next().ok_or(ApiError::NotFound)?;
    Ok(Json(MyServerInfo {
        cidr: format!("{}/{}", s.cidr.network(), s.cidr.prefix()),
        endpoint_host: s.endpoint_host,
        endpoint_port: s.endpoint_port,
        mtu: s.mtu,
    }))
}

// ── Topology positions ──────────────────────────────────────────────────
// Per-user saved node positions for the live-topology drag UI. Stored as
// rows in topology_positions, serialised over the wire as a flat
// {node_id: {x, y}} object — same shape the frontend uses in localStorage.

#[derive(Debug, Serialize, ToSchema)]
pub struct TopologyPositionsResponse {
    pub positions: HashMap<String, Pos>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TopologyPositionsRequest {
    pub positions: HashMap<String, Pos>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TopologyAck {
    #[schema(example = "ok")]
    pub status: &'static str,
    /// Number of (node_id, x, y) rows actually persisted after server-
    /// side sanitisation (non-finite coords + over-long ids are dropped).
    pub count: usize,
}

#[utoipa::path(
    get,
    path = "/me/topology",
    tag = "Account",
    responses(
        (status = 200, description = "Saved node positions for the live topology", body = TopologyPositionsResponse),
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_topology(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let rows = topology_positions::get_all(&state.pool, user.id).await?;
    let mut positions = HashMap::new();
    for r in rows {
        // Drop non-finite values defensively — should never happen since
        // we reject them on write, but a bad row from an older client
        // shouldn't poison the chart.
        if !r.x.is_finite() || !r.y.is_finite() {
            continue;
        }
        positions.insert(r.node_id, Pos { x: r.x, y: r.y });
    }
    Ok(Json(TopologyPositionsResponse { positions }))
}

#[utoipa::path(
    put,
    path = "/me/topology",
    tag = "Account",
    request_body = TopologyPositionsRequest,
    responses(
        (status = 200, description = "Positions persisted (count = rows actually written)", body = TopologyAck),
        (status = 400, description = "Too many entries"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_topology(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<TopologyPositionsRequest>,
) -> ApiResult<impl IntoResponse> {
    // Sanitize: skip non-finite coords, cap the number of entries so a
    // misbehaving client can't fill our table with a million bogus rows.
    const MAX_ENTRIES: usize = 1024;
    if body.positions.len() > MAX_ENTRIES {
        return Err(ApiError::Validation(format!(
            "too many positions ({} > {})",
            body.positions.len(),
            MAX_ENTRIES
        )));
    }
    let mut clean: Vec<topology_positions::Position> = Vec::with_capacity(body.positions.len());
    for (node_id, pos) in body.positions {
        if !pos.x.is_finite() || !pos.y.is_finite() {
            continue;
        }
        // node_id length cap so a malicious client can't bloat rows with
        // arbitrarily long strings. UUID (36) + sentinel ("__hub__") fit
        // comfortably below 64.
        if node_id.is_empty() || node_id.len() > 64 {
            continue;
        }
        clean.push(topology_positions::Position {
            node_id,
            x: pos.x,
            y: pos.y,
        });
    }
    topology_positions::replace_all(&state.pool, user.id, &clean).await?;
    Ok(Json(json!({ "status": "ok", "count": clean.len() })))
}

/// Returns the user's settings-page preferences. Falls through to
/// defaults for users who've never saved — the next PUT creates the row.
#[utoipa::path(
    get,
    path = "/me/preferences",
    tag = "Account",
    responses(
        (status = 200, description = "Current preferences (defaults if never saved)", body = user_prefs::UserPreferences),
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_preferences(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let prefs = user_prefs::get(&state.pool, user.id).await?;
    Ok(Json(prefs))
}

/// The signed-in user's own account-level monthly bandwidth quota: usage, cap,
/// and the next reset. Powers the dashboard "Quota" card — the self-service
/// equivalent of the admin user-detail quota (which only admins can see).
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageResponse {
    /// Bytes used in the current monthly cycle (rx + tx, all devices), treated
    /// as 0 once the cycle rolls over even before the next byte re-zeroes it.
    pub current_month_bytes: i64,
    /// Account monthly byte cap. `null` means unlimited (no cap set, or 0).
    pub monthly_byte_cap: Option<i64>,
    /// Instant the current cycle resets — first of next month, UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub quota_resets_at: OffsetDateTime,
}

#[utoipa::path(
    get,
    path = "/me/usage",
    tag = "Account",
    responses(
        (status = 200, description = "Account monthly quota usage", body = UsageResponse),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn usage(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let now = OffsetDateTime::now_utc();
    let cycle_start = users::first_of_month(now);
    // Derive this month's usage from the trustworthy per-device LIFETIME totals
    // (the device cards' "Total"), not the drift-prone monthly accumulators or
    // aggregates: used = (lifetime now) − (lifetime at this cycle's start). It's
    // therefore always ≤ Total usage and resets cleanly each month.
    let lifetime_total = devices::user_lifetime_total(&state.pool, user.id).await?;
    let (cap, baseline, baseline_at) = users::month_quota(&state.pool, user.id)
        .await?
        .unwrap_or((None, 0, None));

    let used = match baseline_at {
        // Baseline belongs to the current cycle — usage is growth since then.
        Some(at) if at >= cycle_start => (lifetime_total - baseline).max(0),
        // A previous cycle's baseline → rolled into a new month. Re-snapshot to
        // the current lifetime so the new month starts at 0 and grows from here.
        Some(_) => {
            let _ = users::set_month_baseline(&state.pool, user.id, lifetime_total, cycle_start)
                .await;
            0
        }
        // Never initialised — seed a 0 baseline so this first tracked month
        // counts current lifetime (matching the "Total usage" the user sees);
        // the next rollover re-snapshots and accurate monthly tracking begins.
        None => {
            let _ = users::set_month_baseline(&state.pool, user.id, 0, cycle_start).await;
            lifetime_total.max(0)
        }
    };

    Ok(Json(UsageResponse {
        current_month_bytes: used,
        // Cap is config; the stored value is authoritative. 0/None = unlimited.
        monthly_byte_cap: cap.filter(|c| *c > 0),
        quota_resets_at: users::first_of_next_month(now),
    }))
}

// ── Activity log (self-service) ───────────────────────────────────────────
// The user-facing equivalent of the admin audit log, scoped to the signed-in
// user's own activity (actions they took + admin actions taken on their
// account). Paginated so the dashboard's "Recent activity" can link to a full
// "view all" record.

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct ActivityQuery {
    /// Page size. Clamped to [1, 200] server-side. Defaults to 50.
    pub limit: Option<i64>,
    /// Row offset for pagination. Defaults to 0.
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityRow {
    pub id: i64,
    /// Dotted action key, e.g. `device.created`, `auth.login`.
    pub action: String,
    pub target_type: Option<String>,
    #[schema(value_type = Option<String>)]
    pub target_id: Option<uuid::Uuid>,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityPage {
    pub items: Vec<ActivityRow>,
    /// Total entries in scope (for the pagination control).
    pub total: i64,
}

#[utoipa::path(
    get,
    path = "/me/activity",
    tag = "Account",
    params(ActivityQuery),
    responses(
        (status = 200, description = "The user's own activity, newest first", body = ActivityPage),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn activity(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);
    let rows = audit::list_for_user_paged(&state.pool, user.id, limit, offset).await?;
    let total = audit::count_for_user(&state.pool, user.id).await?;
    let items = rows
        .into_iter()
        .map(|r| ActivityRow {
            id: r.id,
            action: r.action,
            target_type: r.target_type,
            target_id: r.target_id,
            metadata: r.metadata,
            created_at: r.created_at,
        })
        .collect();
    Ok(Json(ActivityPage { items, total }))
}

/// Partial update of the user's preferences. Validates the constrained
/// text fields against the same set the DB CHECK enforces so we can
/// surface a clean 400 instead of a sqlx error if a future client sends
/// a stray value.
#[utoipa::path(
    put,
    path = "/me/preferences",
    tag = "Account",
    request_body = user_prefs::UserPreferencesPatch,
    responses(
        (status = 200, description = "Patched preferences (server returns the full merged state)", body = user_prefs::UserPreferences),
        (status = 400, description = "Validation error (unknown enum value)"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_preferences(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(patch): Json<user_prefs::UserPreferencesPatch>,
) -> ApiResult<impl IntoResponse> {
    if let Some(v) = patch.units.as_deref()
        && !matches!(v, "bps" | "Bps") {
            return Err(ApiError::Validation(format!("invalid units: {v}")));
        }
    if let Some(v) = patch.date_format.as_deref()
        && !matches!(v, "iso" | "us" | "eu") {
            return Err(ApiError::Validation(format!("invalid date_format: {v}")));
        }
    if let Some(v) = patch.time_format.as_deref()
        && !matches!(v, "h24" | "h12") {
            return Err(ApiError::Validation(format!("invalid time_format: {v}")));
        }
    if let Some(v) = patch.default_landing.as_deref()
        && !matches!(v, "dashboard" | "devices" | "topology") {
            return Err(ApiError::Validation(format!(
                "invalid default_landing: {v}"
            )));
        }
    if let Some(v) = patch.toast_position.as_deref()
        && !matches!(
            v,
            "top-left"
                | "top-center"
                | "top-right"
                | "bottom-left"
                | "bottom-center"
                | "bottom-right"
        ) {
            return Err(ApiError::Validation(format!(
                "invalid toast_position: {v}"
            )));
        }
    if let Some(v) = patch.theme.as_deref()
        && !matches!(v, "swiss" | "brutalist" | "terminal" | "editorial" | "soft") {
            return Err(ApiError::Validation(format!("invalid theme: {v}")));
        }
    let prefs = user_prefs::upsert(&state.pool, user.id, &patch).await?;
    Ok(Json(prefs))
}


/// "Sign out everywhere" — bumps the user's password-watermark which
/// invalidates every session except the current one (we re-sync the
/// snapshot in this session so the caller stays signed in). No password
/// change happens; the hash is untouched. Used by the Security panel.
#[utoipa::path(
    post,
    path = "/me/sessions/revoke-all",
    tag = "Account",
    responses(
        (status = 200, description = "All other sessions invalidated; this session kept alive", body = StatusAck),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn revoke_other_sessions(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    session: Session,
) -> ApiResult<impl IntoResponse> {
    users::kill_all_sessions(&state.pool, user.id).await?;
    if let Some(new_watermark) = users::find_password_changed_at(&state.pool, user.id).await? {
        session
            .insert(SESSION_KEY_PW_CHANGED_AT, new_watermark.unix_timestamp())
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    // The watermark above already makes other sessions unusable; also
    // hard-delete their rows so the Active-sessions list agrees at once.
    if let Some(session_id) = session.id() {
        user_sessions::revoke_all_except(&state.pool, user.id, &session_id.to_string()).await?;
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.sessions_revoked",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, "user revoked all other sessions");
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MySession {
    #[serde(flatten)]
    pub row: user_sessions::UserSessionRow,
    /// True for the session making this request.
    pub current: bool,
}

/// The caller's live sessions, newest activity first — the data behind the
/// Security page's "Active sessions" panel. Only surrogate ids are exposed;
/// the underlying tower session id is bearer-equivalent and never leaves
/// the server.
#[utoipa::path(
    get,
    path = "/me/sessions",
    tag = "Account",
    responses(
        (status = 200, description = "Live sessions for the caller", body = Vec<MySession>),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    session: Session,
) -> ApiResult<impl IntoResponse> {
    let current_surrogate = match session.id() {
        Some(id) => {
            user_sessions::surrogate_for_session(&state.pool, &id.to_string()).await?
        }
        None => None,
    };
    let rows = user_sessions::list_active_for_user(&state.pool, user.id).await?;
    let out: Vec<MySession> = rows
        .into_iter()
        .map(|row| MySession {
            current: Some(row.id) == current_surrogate,
            row,
        })
        .collect();
    Ok(Json(out))
}

/// Revoke one of the caller's sessions by surrogate id. Deleting the
/// authoritative session row kills that cookie instantly. Revoking the
/// *current* session is allowed and behaves like logout.
#[utoipa::path(
    delete,
    path = "/me/sessions/{id}",
    tag = "Account",
    params(("id" = Uuid, Path, description = "Session surrogate id")),
    responses(
        (status = 200, description = "Session revoked", body = StatusAck),
        (status = 401, description = "No session"),
        (status = 404, description = "No such session for this user"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn revoke_session(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(surrogate): Path<uuid::Uuid>,
) -> ApiResult<impl IntoResponse> {
    let revoked = user_sessions::revoke_by_surrogate(&state.pool, user.id, surrogate).await?;
    if !revoked {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.session_revoked",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: json!({ "session": surrogate }),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, %surrogate, "user revoked one session");
    Ok(Json(json!({ "status": "ok" })))
}

#[utoipa::path(
    delete,
    path = "/me/account",
    tag = "Account",
    responses(
        (status = 200, description = "Account soft-deleted, devices revoked, session flushed", body = StatusAck),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete_account(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    session: Session,
) -> ApiResult<impl IntoResponse> {
    users::soft_delete(&state.pool, user.id).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "user.deleted",
            target_type: Some("user"),
            target_id: Some(user.id),
            metadata: serde_json::json!({}),
            ip: None,
        },
    )
    .await?;
    let _ = session.flush().await;

    // Tear down the live WG peers AND release device IPs from the
    // in-memory allocator. Both are best-effort: log on failure but
    // never abort the deletion (DB is already updated).
    if let Ok(user_devices) = devices::list_for_user(&state.pool, user.id).await {
        for d in user_devices {
            if d.status == zerovpn_core::models::DeviceStatus::Active
                && let Err(e) = state.wg.remove_peer(&d.public_key).await {
                    tracing::warn!(?e, device_id = %d.id, "delete_account: wg remove_peer failed");
                }
            if let Some(alloc) = state.allocators.get(d.server_id) {
                let _ = alloc.release(d.allocated_ip.ip());
            }
        }
    }

    info!(user_id = %user.id, "account soft-deleted");
    Ok(Json(json!({ "status": "ok" })))
}
