use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use tower_sessions::Session;
use tracing::{info, warn};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use zerovpn_core::models::{Server, UserRole, UserStatus};
use zerovpn_db::repos::{
    access_logs, audit, bandwidth, connection_sessions, devices, peer_endpoint_history,
    servers, session_events,
    users::{self, AdminUserFilters},
};
use zerovpn_wire::{ChangeAction, Event, ResourceKind};

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::{
        RequireAdmin, SESSION_KEY_PW_CHANGED_AT, SESSION_KEY_REAL_PW_CHANGED_AT,
        SESSION_KEY_REAL_USER_ID, SESSION_KEY_USER_ID,
    },
    routes::{
        devices::{PERSISTENT_KEEPALIVE, PublicDevice},
        dto::StatusAck,
    },
    state::AppState,
};

// ── Real-time cross-session notifications ───────────────────────────────
/// Broadcast a change to a specific user's account/devices so that user's
/// own sessions (e.g. an admin acting on themselves) and every admin
/// console refresh in real time. `action` only drives optional client toasts.
fn notify_user(state: &AppState, target_user_id: Uuid, action: ChangeAction) {
    state.broadcast(Event::DataChanged {
        user_id: Some(target_user_id),
        resource: ResourceKind::User,
        id: Some(target_user_id),
        action,
    });
}

/// Broadcast an admin-global change (server config, maintenance mode). Only
/// admins receive these — `visible_to` drops `user_id == None` for non-admins.
fn notify_admin(state: &AppState, resource: ResourceKind, id: Option<Uuid>, action: ChangeAction) {
    state.broadcast(Event::DataChanged {
        user_id: None,
        resource,
        id,
        action,
    });
}

// ── WireGuard sync helpers ──────────────────────────────────────────────
// Used whenever an account-level lifecycle change should ripple out to
// the live WG interface — suspend/unsuspend/delete on a user. Each
// helper is best-effort: we log peer-level failures but never abort the
// account-level operation, so the DB always converges even if WG is
// temporarily unreachable. The reconciler picks up any drift later.

/// Drop every active peer that belongs to this user from the live WG
/// interface. Use on suspend / delete to actually disconnect the user
/// instead of just blocking their dashboard access.
async fn remove_user_peers(state: &AppState, user_id: Uuid) {
    let user_devices = match devices::list_for_user(&state.pool, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(?e, %user_id, "remove_user_peers: list_for_user failed");
            return;
        }
    };
    for d in user_devices {
        // Only `Active` devices have a peer registered on the WG box —
        // `Paused` devices were already torn down at pause time.
        if d.status != zerovpn_core::models::DeviceStatus::Active {
            continue;
        }
        if let Err(e) = state.wg.remove_peer(&d.public_key).await {
            warn!(?e, device_id = %d.id, "remove_user_peers: wg remove_peer failed");
        }
    }
}

/// Re-add every active peer that belongs to this user to the live WG
/// interface. Use on unsuspend so previously-suspended users can
/// reconnect immediately without rotating keys or re-downloading the
/// .conf. Paused devices stay paused.
async fn restore_user_peers(state: &AppState, user_id: Uuid) {
    let user_devices = match devices::list_for_user(&state.pool, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(?e, %user_id, "restore_user_peers: list_for_user failed");
            return;
        }
    };
    // Cache the per-server keepalive across this user's devices (they often
    // share a server) so we don't refetch the same row N times.
    let mut keepalive_cache: std::collections::HashMap<Uuid, u16> =
        std::collections::HashMap::new();
    for d in user_devices {
        if d.status != zerovpn_core::models::DeviceStatus::Active {
            continue;
        }
        let keepalive = match keepalive_cache.get(&d.server_id) {
            Some(v) => *v,
            None => {
                let v = match servers::find_by_id(&state.pool, d.server_id).await {
                    Ok(Some(s)) => s.persistent_keepalive as u16,
                    Ok(None) | Err(_) => PERSISTENT_KEEPALIVE,
                };
                keepalive_cache.insert(d.server_id, v);
                v
            }
        };
        if let Err(e) = state
            .wg
            .add_peer(&d.public_key, d.allocated_ip.ip(), None, keepalive)
            .await
        {
            warn!(?e, device_id = %d.id, "restore_user_peers: wg add_peer failed");
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListQuery {
    /// Free-text email search (substring, case-insensitive).
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Filter by account status. Omit to include every status.
    #[serde(default)]
    pub status: Option<UserStatus>,
    /// Filter by role. Omit to include every role.
    #[serde(default)]
    pub role: Option<UserRole>,
    /// Filter by 2FA enrollment. Omit to ignore.
    #[serde(default)]
    pub totp_enabled: Option<bool>,
}
fn default_limit() -> i64 { 50 }

impl ListQuery {
    fn filters(&self) -> AdminUserFilters<'_> {
        AdminUserFilters {
            search: self.q.as_deref(),
            status: self.status,
            role: self.role,
            totp_enabled: self.totp_enabled,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUser {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
    pub device_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserList {
    pub total: i64,
    pub items: Vec<AdminUser>,
}

#[utoipa::path(
    get,
    path = "/admin/users",
    tag = "Admin",
    params(ListQuery),
    responses(
        (status = 200, description = "Paginated user list", body = AdminUserList),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_users(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Query(q): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);
    let f = q.filters();
    let total = users::admin_count(&state.pool, f).await?;
    let rows = users::admin_list(&state.pool, limit, offset, f).await?;
    let items = rows
        .into_iter()
        .map(|u| AdminUser {
            id: u.id,
            email: u.email,
            role: u.role,
            status: u.status,
            totp_enabled: u.totp_enabled,
            created_at: u.created_at,
            last_login_at: u.last_login_at,
            device_count: u.device_count,
        })
        .collect();
    Ok(Json(AdminUserList { total, items }))
}

/// CSV export of the filtered user list. Same filters as `list_users`,
/// but returns the full result (capped at 10000 rows) so an admin can
/// hand the file off for spreadsheet review.
#[utoipa::path(
    get,
    path = "/admin/users.csv",
    tag = "Admin",
    params(ListQuery),
    responses(
        (status = 200, description = "CSV download", content_type = "text/csv"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_users_csv(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Query(q): Query<ListQuery>,
) -> ApiResult<axum::response::Response> {
    let f = q.filters();
    let rows = users::admin_list(&state.pool, 10_000, 0, f).await?;

    let mut buf = Vec::with_capacity(80 * rows.len());
    {
        let mut wtr = csv::Writer::from_writer(&mut buf);
        wtr.write_record([
            "id",
            "email",
            "role",
            "status",
            "totp_enabled",
            "created_at",
            "last_login_at",
            "device_count",
        ])
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        for u in rows {
            // Enum serde already lowercases ("admin"/"user", "active"/...);
            // unwrap to the inner string to avoid the Debug-format trick.
            let role_s = serde_json::to_value(u.role)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            let status_s = serde_json::to_value(u.status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            wtr.write_record([
                u.id.to_string(),
                u.email,
                role_s,
                status_s,
                u.totp_enabled.to_string(),
                u.created_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                u.last_login_at
                    .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
                    .unwrap_or_default(),
                u.device_count.to_string(),
            ])
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        wtr.flush().map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    use axum::http::header;
    let resp = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"users.csv\"",
        )
        .body(axum::body::Body::from(buf))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(resp)
}

// ---- User detail --------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserDetail {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_enabled: bool,
    pub must_change_password: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub email_verified_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub password_changed_at: OffsetDateTime,
    pub current_month_bytes: i64,
    pub monthly_byte_cap: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub quota_resets_at: Option<OffsetDateTime>,
    pub device_count: i64,
    /// Max active (non-revoked) devices this user may create (admin-settable).
    pub device_limit: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserDevice {
    pub id: Uuid,
    pub name: String,
    pub os: zerovpn_core::models::DeviceOs,
    pub status: zerovpn_core::models::DeviceStatus,
    pub allocated_ip: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_handshake_at: Option<OffsetDateTime>,
    /// Most recent `host:port` the peer connected from, as observed by
    /// the WG poller. `None` until the device's first handshake.
    pub last_peer_endpoint: Option<String>,
    /// Wall-clock time the `last_peer_endpoint` was first observed.
    /// Updated together with the endpoint on every change.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_peer_endpoint_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Per-device monthly byte cap (null/0 = no device cap; the account cap
    /// still applies). Enforced alongside the account cap by the quota sweep.
    pub monthly_byte_cap: Option<i64>,
    /// The device's own bandwidth used this monthly cycle.
    pub current_month_bytes: i64,
    /// True when the quota sweep paused this device (device or account cap),
    /// as opposed to a manual user pause — drives the "auto-paused" hint.
    pub auto_paused: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserActivity {
    pub id: i64,
    pub action: String,
    pub metadata: serde_json::Value,
    /// Full client IP captured when the audit row was written. `null`
    /// for rows emitted from worker tasks / CLI / state transitions.
    #[schema(value_type = Option<String>, example = "203.0.113.42/32")]
    pub ip: Option<IpNetwork>,
    /// Raw `User-Agent` header. `null` for the same set as `ip`.
    pub user_agent: Option<String>,
    /// `"user"` / `"device"` / `"server"` / etc. — surface what the
    /// audit row was *about* so the timeline can render "edited device
    /// X" not just "device.updated".
    pub target_type: Option<String>,
    pub target_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserDetailResponse {
    pub user: AdminUserDetail,
    pub devices: Vec<AdminUserDevice>,
    /// Recent audit entries where this user is either the *actor* (own
    /// actions) or the *target* (admin actions taken on them). Newest
    /// first, hard-capped at 100. Drives the unified per-user activity
    /// timeline together with `session_events` and `connection_sessions`.
    pub activity: Vec<AdminUserActivity>,
    /// Recent `session_events` rows scoped to this user (logins,
    /// logouts, 2FA toggles, impersonations, etc.). Newest first,
    /// hard-capped at 50.
    pub session_events: Vec<session_events::SessionEventRow>,
    /// Recent `connection_sessions` rows across every one of this
    /// user's devices. Newest first, hard-capped at 50. Lets the
    /// timeline render "device A came online at HH:MM, disconnected
    /// after 12m, 50MB up" inline.
    pub connection_sessions: Vec<connection_sessions::ConnectionSessionRow>,
}

#[utoipa::path(
    get,
    path = "/admin/users/{id}",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target user UUID"),
    ),
    responses(
        (status = 200, description = "Bundled user detail (core + quota + devices + recent activity)", body = AdminUserDetailResponse),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn user_detail(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(target_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let row = users::admin_user_detail(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // Admin device list. Includes the Phase 2 / Stage A peer-endpoint
    // columns, which the core `Device` model doesn't carry — sidestepping
    // a cascade of SELECT rewrites by pulling exactly what the admin UI
    // needs as a one-off shape here.
    #[derive(sqlx::FromRow)]
    struct DeviceRow {
        id: Uuid,
        name: String,
        os: zerovpn_core::models::DeviceOs,
        status: zerovpn_core::models::DeviceStatus,
        allocated_ip: ipnetwork::IpNetwork,
        last_handshake_at: Option<OffsetDateTime>,
        last_peer_endpoint: Option<String>,
        last_peer_endpoint_at: Option<OffsetDateTime>,
        created_at: OffsetDateTime,
        monthly_byte_cap: Option<i64>,
        current_month_bytes: i64,
        auto_paused: bool,
    }
    let device_rows: Vec<DeviceRow> = sqlx::query_as(
        r#"SELECT id, name, os, status, allocated_ip,
                  last_handshake_at, last_peer_endpoint, last_peer_endpoint_at,
                  created_at, monthly_byte_cap, current_month_bytes, auto_paused
             FROM devices
            WHERE user_id = $1 AND status <> 'revoked'
            ORDER BY display_order NULLS LAST, created_at DESC"#,
    )
    .bind(target_id)
    .fetch_all(&state.pool)
    .await?;
    let devices: Vec<AdminUserDevice> = device_rows
        .into_iter()
        .map(|d| AdminUserDevice {
            id: d.id,
            name: d.name,
            os: d.os,
            status: d.status,
            allocated_ip: d.allocated_ip.ip().to_string(),
            last_handshake_at: d.last_handshake_at,
            last_peer_endpoint: d.last_peer_endpoint,
            last_peer_endpoint_at: d.last_peer_endpoint_at,
            created_at: d.created_at,
            monthly_byte_cap: d.monthly_byte_cap,
            current_month_bytes: d.current_month_bytes,
            auto_paused: d.auto_paused,
        })
        .collect();

    // Phase 2 / Stage B — richer per-user audit feed: includes rows
    // where this user is the ACTOR (their own actions), not just rows
    // where they're the target. Carries IP / UA / target so the
    // unified timeline can render the full context inline.
    let activity_rows = audit::list_for_user(&state.pool, target_id, 100).await?;
    let activity = activity_rows
        .into_iter()
        .map(|a| AdminUserActivity {
            id: a.id,
            action: a.action,
            metadata: a.metadata,
            ip: a.ip,
            user_agent: a.user_agent,
            target_type: a.target_type,
            target_id: a.target_id,
            created_at: a.created_at,
        })
        .collect();

    // Per-user session events — same shape the cross-fleet
    // /admin/session-events page renders, filtered to this user.
    let session_events_rows = session_events::list_recent(
        &state.pool,
        session_events::Filters {
            user_id: Some(target_id),
            ..Default::default()
        },
        50,
        0,
    )
    .await?;

    // Per-user connection sessions across every one of their devices.
    // Feeds the unified activity timeline; admins can also drill into
    // a single device for the per-device list via /admin/devices/:id.
    let connection_sessions_rows =
        connection_sessions::list_for_user(&state.pool, target_id, 50).await?;

    Ok(Json(AdminUserDetailResponse {
        user: AdminUserDetail {
            id: row.id,
            email: row.email,
            role: row.role,
            status: row.status,
            totp_enabled: row.totp_enabled,
            must_change_password: row.must_change_password,
            created_at: row.created_at,
            last_login_at: row.last_login_at,
            email_verified_at: row.email_verified_at,
            password_changed_at: row.password_changed_at,
            current_month_bytes: row.current_month_bytes,
            monthly_byte_cap: row.monthly_byte_cap,
            quota_resets_at: row.quota_resets_at,
            device_count: row.device_count,
            device_limit: row.device_limit,
        },
        devices,
        activity,
        session_events: session_events_rows,
        connection_sessions: connection_sessions_rows,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StatusBody {
    pub status: UserStatus,
}

#[utoipa::path(
    put,
    path = "/admin/users/{id}/status",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target user UUID"),
    ),
    request_body = StatusBody,
    responses(
        (status = 200, description = "Status updated", body = StatusAck),
        (status = 400, description = "Cannot change your own status"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_user_status(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> ApiResult<impl IntoResponse> {
    if target_id == actor.id {
        return Err(ApiError::Validation("cannot change your own status".into()));
    }
    // Snapshot the prior status so we know which side of the transition
    // we're on — needed to decide whether to tear down or restore peers.
    let prior = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?
        .status;
    if prior == UserStatus::PendingVerification || body.status == UserStatus::PendingVerification {
        return Err(ApiError::Validation(
            "pending invitations can only be activated by email verification and Google sign-in".into(),
        ));
    }
    let n = users::admin_set_status(&state.pool, target_id, body.status).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }

    // Sync the live WG interface to the new status. Suspending a user
    // only mattered to the dashboard before this — the tunnel itself
    // stayed up because peers were never removed. These two helpers
    // close that gap.
    if body.status == UserStatus::Suspended && prior != UserStatus::Suspended {
        remove_user_peers(&state, target_id).await;
        // Flush any open dashboard sessions: the auth extractor's
        // status check already returns 403 on the next request, but
        // bumping the pw watermark also kills the cookie's pw snapshot
        // so it can't ride a stale-but-valid session through anywhere
        // we *don't* gate on status (eg. prior to maintenance gating).
        let _ = users::kill_all_sessions(&state.pool, target_id).await;
    } else if body.status == UserStatus::Active && prior == UserStatus::Suspended {
        restore_user_peers(&state, target_id).await;
    }

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_status_changed",
            target_type: Some("user"),
            target_id: Some(target_id),
            metadata: json!({ "status": body.status, "prior": prior }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target_id, status = ?body.status, "admin set user status");
    notify_user(&state, target_id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Role change ----------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct RoleBody {
    pub role: UserRole,
}

#[utoipa::path(
    put,
    path = "/admin/users/{id}/role",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    request_body = RoleBody,
    responses(
        (status = 200, description = "Role updated", body = StatusAck),
        (status = 400, description = "Cannot demote yourself / cannot demote the last admin"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_user_role(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Json(body): Json<RoleBody>,
) -> ApiResult<impl IntoResponse> {
    if target_id == actor.id {
        return Err(ApiError::Validation("cannot change your own role".into()));
    }
    // Don't strand the deployment without an admin: the last active admin
    // can't be demoted. Bootstrap can always re-promote via direct DB
    // access if everyone leaves.
    if body.role == UserRole::User {
        let target = users::find_by_id(&state.pool, target_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        // Only an *active* admin counts toward the floor — a suspended /
        // pending admin holds no live access, so demoting one can't strand the
        // deployment (mirrors the delete guard).
        if target.role == UserRole::Admin && target.status == UserStatus::Active {
            let admins = users::count_active_admins(&state.pool).await?;
            if admins <= 1 {
                return Err(ApiError::Validation(
                    "cannot demote the last remaining admin".into(),
                ));
            }
        }
    }
    let n = users::admin_set_role(&state.pool, target_id, body.role).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_role_changed",
            target_type: Some("user"),
            target_id: Some(target_id),
            metadata: json!({ "role": body.role }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target_id, role = ?body.role, "admin set user role");
    notify_user(&state, target_id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}


// ---- Disable 2FA ----------------------------------------------------------

#[utoipa::path(
    post,
    path = "/admin/users/{id}/disable-2fa",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    responses(
        (status = 200, description = "TOTP cleared and recovery codes wiped", body = StatusAck),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_disable_2fa(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let target = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    users::disable_totp(&state.pool, target.id).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_2fa_disabled",
            target_type: Some("user"),
            target_id: Some(target.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target.id, "admin cleared TOTP");
    notify_user(&state, target.id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Force-logout (revoke all sessions) ----------------------------------

#[utoipa::path(
    post,
    path = "/admin/users/{id}/sessions/revoke-all",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    responses(
        (status = 200, description = "All open sessions for this user invalidated", body = StatusAck),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_revoke_sessions(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let target = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let n = users::kill_all_sessions(&state.pool, target.id).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_sessions_revoked",
            target_type: Some("user"),
            target_id: Some(target.id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target.id, "admin revoked all sessions");
    notify_user(&state, target.id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Edit email ----------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct EmailBody {
    pub email: String,
}

#[utoipa::path(
    put,
    path = "/admin/users/{id}/email",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    request_body = EmailBody,
    responses(
        (status = 200, description = "Email updated", body = StatusAck),
        (status = 400, description = "Validation failed (bad shape, taken, deleted user)"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_set_email_route(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Json(body): Json<EmailBody>,
) -> ApiResult<impl IntoResponse> {
    let new_email = body.email.trim().to_lowercase();
    if !new_email.contains('@') || new_email.len() < 3 {
        return Err(ApiError::Validation("invalid email shape".into()));
    }
    let target = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.status == UserStatus::Deleted {
        return Err(ApiError::Validation("user is deleted".into()));
    }
    if target.status == UserStatus::PendingVerification {
        return Err(ApiError::Validation("delete and re-invite a pending user to change their email".into()));
    }
    if target.email.to_lowercase() == new_email {
        // No-op: same email after normalisation. Don't write or audit.
        return Ok(Json(json!({ "status": "ok" })));
    }
    if let Some(other) = users::find_by_email(&state.pool, &new_email).await?
        && other.id != target_id {
            return Err(ApiError::Validation("email already in use".into()));
        }
    let n = users::admin_set_email(&state.pool, target.id, &new_email).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_email_changed",
            target_type: Some("user"),
            target_id: Some(target.id),
            metadata: json!({ "from": target.email, "to": new_email }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target.id, "admin changed email");
    notify_user(&state, target.id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Delete user ----------------------------------------------------------

#[utoipa::path(
    delete,
    path = "/admin/users/{id}",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    responses(
        (status = 200, description = "User permanently deleted: peers removed + every row tied to the user purged (devices, sessions, logs, bandwidth, prefs, …). Irreversible.", body = StatusAck),
        (status = 400, description = "Cannot delete yourself / last admin"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete_user(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    if target_id == actor.id {
        return Err(ApiError::Validation("cannot delete yourself".into()));
    }
    let target = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Only an *active* admin counts toward the "keep at least one admin" floor.
    // A suspended / pending (e.g. never-verified invite) admin holds no live
    // access — `count_active_admins` already excludes them — so deleting one
    // can't strand the deployment. Without this status check you couldn't
    // delete such an admin while you were the only active one.
    if target.role == UserRole::Admin && target.status == UserStatus::Active {
        let admins = users::count_active_admins(&state.pool).await?;
        if admins <= 1 {
            return Err(ApiError::Validation(
                "cannot delete the last remaining admin".into(),
            ));
        }
    }
    // Tear down the live WG peers AND release IPs BEFORE the purge — the
    // hard delete drops the device rows, so we must read them while they
    // still exist. Best-effort: failures log but don't block the deletion.
    if let Ok(user_devices) = devices::list_for_user(&state.pool, target.id).await {
        for d in user_devices {
            if d.status == zerovpn_core::models::DeviceStatus::Active
                && let Err(e) = state.wg.remove_peer(&d.public_key).await {
                    warn!(?e, device_id = %d.id, "delete_user: wg remove_peer failed");
                }
            if let Some(alloc) = state.allocators.get(d.server_id) {
                let _ = alloc.release(d.allocated_ip.ip());
            }
        }
    }
    // Permanently purge the user + every row tied to them (devices cascade,
    // logs/samples/failed-logins purged explicitly). The user row is gone
    // afterward, so all their sessions die on the next request automatically.
    users::hard_delete(&state.pool, target.id, &target.email).await?;
    // Recorded AFTER the purge (which deletes audit rows referencing the
    // target) so this deletion record survives as the accountability trail.
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_deleted",
            target_type: Some("user"),
            target_id: Some(target.id),
            metadata: json!({ "email": target.email, "hard": true }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target.id, "admin hard-deleted user");
    notify_user(&state, target.id, ChangeAction::Deleted);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Create / invite user ------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateUserBody {
    pub email: String,
    /// Default `user`. Set to `admin` to create another administrator.
    #[serde(default = "default_role")]
    pub role: UserRole,
}

fn default_role() -> UserRole {
    UserRole::User
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedUserResponse {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
}

#[utoipa::path(
    post,
    path = "/admin/users",
    tag = "Admin",
    request_body = CreateUserBody,
    responses(
        (status = 200, description = "User created", body = CreatedUserResponse),
        (status = 400, description = "Validation error / email already taken"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn create_user(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Json(body): Json<CreateUserBody>,
) -> ApiResult<impl IntoResponse> {
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') {
        return Err(ApiError::Validation("email is required and must contain @".into()));
    }
    if let Some(existing) = users::find_by_email(&state.pool, &email).await? {
        if existing.status == UserStatus::PendingVerification {
            let live: Option<(Uuid,)> = sqlx::query_as(
                "SELECT user_id FROM invitations WHERE user_id = $1 AND revoked_at IS NULL AND accepted_at IS NULL",
            ).bind(existing.id).fetch_optional(&state.pool).await?;
            if live.is_none() {
                sqlx::query("UPDATE users SET role = $2 WHERE id = $1 AND status = 'pending_verification'")
                    .bind(existing.id).bind(body.role).execute(&state.pool).await?;
                super::invitations::issue(&state, existing.id, Some(actor.id), &email).await?;
                audit::record(&state.pool, audit::AuditEntry {
                    actor_user_id: Some(actor.id), action: "admin.invitation_reissued",
                    target_type: Some("user"), target_id: Some(existing.id),
                    metadata: json!({}), ip: None,
                }).await?;
                return Ok(Json(CreatedUserResponse {
                    id: existing.id, email, role: body.role, status: existing.status,
                }));
            }
        }
        return Err(ApiError::Validation("email already in use".into()));
    }

    // Password authentication is disabled. `!` is deliberately not a valid
    // Argon2 hash; the invited user must verify their email then use Google.
    let id = users::create(
        &state.pool,
        &email,
        "!",
        body.role,
        UserStatus::PendingVerification,
    )
    .await?;
    super::invitations::issue(&state, id, Some(actor.id), &email).await?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.invitation_created",
            target_type: Some("user"),
            target_id: Some(id),
            metadata: json!({
                "role": body.role,
                "invite": true,
            }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %id, role = ?body.role, "admin created user");
    notify_user(&state, id, ChangeAction::Created);

    Ok(Json(CreatedUserResponse {
        id,
        email,
        role: body.role,
        status: UserStatus::PendingVerification,
    }))
}

// ---- User bandwidth history ----------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct BandwidthRangeQuery {
    /// "24h" | "7d" | "30d". Defaults to "24h".
    #[serde(default)]
    pub range: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserBandwidthBucket {
    #[serde(with = "time::serde::rfc3339")]
    pub bucket_start: OffsetDateTime,
    pub rx_bytes: i64,
    pub tx_bytes: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserBandwidthResponse {
    pub bucket: &'static str,
    pub range: String,
    pub buckets: Vec<AdminUserBandwidthBucket>,
}

#[utoipa::path(
    get,
    path = "/admin/users/{id}/bandwidth",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID"), BandwidthRangeQuery),
    responses(
        (status = 200, description = "Bucketed RX/TX history aggregated across the user's devices", body = AdminUserBandwidthResponse),
        (status = 400, description = "Invalid range"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn user_bandwidth(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Query(q): Query<BandwidthRangeQuery>,
) -> ApiResult<impl IntoResponse> {
    let range = q.range.unwrap_or_else(|| "24h".into());
    let (since, bucket) = match range.as_str() {
        "24h" => (OffsetDateTime::now_utc() - time::Duration::hours(24), "hour"),
        "7d" => (OffsetDateTime::now_utc() - time::Duration::days(7), "hour"),
        "30d" => (OffsetDateTime::now_utc() - time::Duration::days(30), "day"),
        other => {
            return Err(ApiError::Validation(format!(
                "range must be 24h | 7d | 30d (got {other})"
            )));
        }
    };
    let rows = bandwidth::user_totals(&state.pool, target_id, since, bucket).await?;
    Ok(Json(AdminUserBandwidthResponse {
        bucket: if bucket == "hour" { "hour" } else { "day" },
        range,
        buckets: rows
            .into_iter()
            .map(|b| AdminUserBandwidthBucket {
                bucket_start: b.bucket_start,
                rx_bytes: b.rx_bytes,
                tx_bytes: b.tx_bytes,
            })
            .collect(),
    }))
}

// ---- Audit log ------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct AuditQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Optional action filter, e.g. "device.created".
    #[serde(default)]
    pub action: Option<String>,
    /// Restrict to entries authored by this user.
    #[serde(default)]
    pub actor_user_id: Option<Uuid>,
    /// Restrict to entries about this target (any target_type).
    #[serde(default)]
    pub target_id: Option<Uuid>,
    /// Restrict to entries with this target_type ("user", "device", "server", …).
    #[serde(default)]
    pub target_type: Option<String>,
    /// RFC3339 lower bound (inclusive). Restricts to created_at ≥ since.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    /// RFC3339 upper bound (exclusive). Restricts to created_at < until.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct AuditRow {
    pub id: i64,
    pub actor_user_id: Option<Uuid>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    /// Full client IP serialised as `IpNetwork` ("203.0.113.42/32").
    /// Stage A — full address, no longer /24-truncated. Renamed from
    /// `ip_prefix` in migration 20.
    #[schema(value_type = Option<String>, example = "203.0.113.42/32")]
    pub ip: Option<IpNetwork>,
    /// Raw `User-Agent` header captured when the audit row was written.
    /// `NULL` for audit rows recorded outside of a route handler
    /// (worker tasks, CLI actions, internal state transitions).
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditList {
    /// Filtered total — count for the active `action` filter, ignoring
    /// `limit` / `offset`. Powers pagination controls on the admin page.
    pub total: i64,
    pub items: Vec<AuditRow>,
}

#[utoipa::path(
    get,
    path = "/admin/audit",
    tag = "Admin",
    params(AuditQuery),
    responses(
        (status = 200, description = "Audit log entries, newest first", body = AuditList),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_audit(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    headers: axum::http::HeaderMap,
    Query(q): Query<AuditQuery>,
) -> ApiResult<impl IntoResponse> {
    let _ = zerovpn_db::repos::audit::record_with_ua(
        &state.pool,
        zerovpn_db::repos::audit::AuditEntry {
            action: "admin_viewed_logs",
            actor_user_id: Some(admin.id),
            target_type: Some("system"),
            target_id: None,
            metadata: serde_json::json!({"path": "list_audit"}),
ip: crate::routes::auth::client_ip(&headers),
        },
        crate::routes::auth::client_user_agent(&headers).as_deref(),
    )
    .await;

    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    // Single param-list shared between count + fetch so the WHERE
    // clauses stay in sync. NULL on a filter param disables it.
    let (total,): (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)::BIGINT FROM audit_logs
            WHERE ($1::TEXT IS NULL OR action        = $1)
              AND ($2::UUID IS NULL OR actor_user_id = $2)
              AND ($3::UUID IS NULL OR target_id     = $3)
              AND ($4::TEXT IS NULL OR target_type   = $4)
              AND ($5::TIMESTAMPTZ IS NULL OR created_at >= $5)
              AND ($6::TIMESTAMPTZ IS NULL OR created_at <  $6)"#,
    )
    .bind(q.action.as_deref())
    .bind(q.actor_user_id)
    .bind(q.target_id)
    .bind(q.target_type.as_deref())
    .bind(q.since)
    .bind(q.until)
    .fetch_one(&state.pool)
    .await?;
    let items: Vec<AuditRow> = sqlx::query_as(
        r#"SELECT id, actor_user_id, action, target_type, target_id, metadata,
                  ip, user_agent, created_at
             FROM audit_logs
            WHERE ($3::TEXT IS NULL OR action        = $3)
              AND ($4::UUID IS NULL OR actor_user_id = $4)
              AND ($5::UUID IS NULL OR target_id     = $5)
              AND ($6::TEXT IS NULL OR target_type   = $6)
              AND ($7::TIMESTAMPTZ IS NULL OR created_at >= $7)
              AND ($8::TIMESTAMPTZ IS NULL OR created_at <  $8)
            ORDER BY id DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(limit)
    .bind(offset)
    .bind(q.action.as_deref())
    .bind(q.actor_user_id)
    .bind(q.target_id)
    .bind(q.target_type.as_deref())
    .bind(q.since)
    .bind(q.until)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(AuditList { total, items }))
}

// ---- Failed logins --------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct FailedLoginRow {
    pub id: i64,
    pub email_attempted: Option<String>,
    pub reason: zerovpn_db::repos::failed_logins::FailedLoginReason,
    /// Full client IP (Phase 2 / Stage A). `/32` or `/128` host
    /// network. Renamed from `ip_prefix` in migration 20.
    #[schema(value_type = Option<String>, example = "203.0.113.42/32")]
    pub ip: Option<IpNetwork>,
    /// Raw `User-Agent` header from the failing request. Plaintext.
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub attempted_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FailedLoginList {
    /// Total failed-login rows in the DB (ignoring `limit` / `offset`).
    /// Drives pagination controls on the admin page.
    pub total: i64,
    pub items: Vec<FailedLoginRow>,
}

/// Audit log as CSV for download.
#[utoipa::path(
    get,
    path = "/admin/audit.csv",
    tag = "Admin",
    params(AuditQuery),
    responses(
        (status = 200, description = "CSV download", content_type = "text/csv"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_audit_csv(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<axum::response::Response> {
    let limit = q.limit.clamp(1, 5000);
    let items: Vec<AuditRow> = sqlx::query_as(
        r#"SELECT id, actor_user_id, action, target_type, target_id, metadata,
                  ip, user_agent, created_at
             FROM audit_logs
            WHERE ($2::TEXT IS NULL OR action        = $2)
              AND ($3::UUID IS NULL OR actor_user_id = $3)
              AND ($4::UUID IS NULL OR target_id     = $4)
              AND ($5::TEXT IS NULL OR target_type   = $5)
              AND ($6::TIMESTAMPTZ IS NULL OR created_at >= $6)
              AND ($7::TIMESTAMPTZ IS NULL OR created_at <  $7)
            ORDER BY id DESC
            LIMIT $1"#,
    )
    .bind(limit)
    .bind(q.action.as_deref())
    .bind(q.actor_user_id)
    .bind(q.target_id)
    .bind(q.target_type.as_deref())
    .bind(q.since)
    .bind(q.until)
    .fetch_all(&state.pool)
    .await?;

    let mut buf = Vec::with_capacity(64 * items.len());
    {
        let mut wtr = csv::Writer::from_writer(&mut buf);
        wtr.write_record([
            "id",
            "actor_user_id",
            "action",
            "target_type",
            "target_id",
            "metadata",
            "ip",
            "user_agent",
            "created_at",
        ])
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        for r in items {
            wtr.write_record([
                r.id.to_string(),
                r.actor_user_id.map(|u| u.to_string()).unwrap_or_default(),
                r.action,
                r.target_type.unwrap_or_default(),
                r.target_id.map(|t| t.to_string()).unwrap_or_default(),
                r.metadata.to_string(),
                r.ip.map(|i| i.to_string()).unwrap_or_default(),
                r.user_agent.unwrap_or_default(),
                r.created_at.unix_timestamp().to_string(),
            ])
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        wtr.flush().map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    use axum::http::header;
    let resp = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"audit.csv\"",
        )
        .body(axum::body::Body::from(buf))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(resp)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct QuotaBody {
    /// Cap in bytes for the current month. Null/0 → unlimited.
    pub monthly_byte_cap: Option<i64>,
}

#[utoipa::path(
    put,
    path = "/admin/users/{id}/quota",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target user UUID"),
    ),
    request_body = QuotaBody,
    responses(
        (status = 200, description = "Quota updated (null/0 = unlimited)", body = StatusAck),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_user_quota(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Json(body): Json<QuotaBody>,
) -> ApiResult<impl IntoResponse> {
    let cap = body
        .monthly_byte_cap
        .filter(|c| *c > 0);
    sqlx::query(
        "UPDATE users SET monthly_byte_cap = $2 WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(target_id)
    .bind(cap)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_quota_set",
            target_type: Some("user"),
            target_id: Some(target_id),
            metadata: json!({ "monthly_byte_cap": cap }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target_id, ?cap, "admin set user quota");
    notify_user(&state, target_id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

/// Bounds for the per-user active-device cap. Min 1 (0 would block all
/// devices — use suspend for that); max keeps a single user from exhausting
/// the shared /22 IP pool.
const DEVICE_LIMIT_MIN: i32 = 1;
const DEVICE_LIMIT_MAX: i32 = 1000;

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeviceLimitBody {
    /// Max active (non-revoked) devices the user may create (1–1000).
    pub device_limit: i32,
}

#[utoipa::path(
    put,
    path = "/admin/users/{id}/device-limit",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target user UUID"),
    ),
    request_body = DeviceLimitBody,
    responses(
        (status = 200, description = "Device limit updated", body = StatusAck),
        (status = 400, description = "device_limit out of range (1–1000)"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "No such user"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_user_device_limit(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_id): Path<Uuid>,
    Json(body): Json<DeviceLimitBody>,
) -> ApiResult<impl IntoResponse> {
    let limit = body.device_limit;
    if !(DEVICE_LIMIT_MIN..=DEVICE_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "device_limit must be between {DEVICE_LIMIT_MIN} and {DEVICE_LIMIT_MAX}"
        )));
    }
    let affected = zerovpn_db::repos::users::set_device_limit(&state.pool, target_id, limit).await?;
    if affected == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_device_limit_set",
            target_type: Some("user"),
            target_id: Some(target_id),
            metadata: json!({ "device_limit": limit }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, target = %target_id, limit, "admin set user device limit");
    notify_user(&state, target_id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

#[utoipa::path(
    put,
    path = "/admin/devices/{id}/quota",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target device UUID"),
    ),
    request_body = QuotaBody,
    responses(
        (status = 200, description = "Device quota updated (null/0 = no device cap)", body = StatusAck),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Device not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_device_quota(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(device_id): Path<Uuid>,
    Json(body): Json<QuotaBody>,
) -> ApiResult<impl IntoResponse> {
    let device = devices::get_by_id(&state.pool, device_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let cap = body.monthly_byte_cap.filter(|c| *c > 0);
    devices::set_quota(&state.pool, device_id, cap).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.device_quota_set",
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({ "monthly_byte_cap": cap }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, %device_id, ?cap, "admin set device quota");
    // Surface on the owner's account + admin consoles (the device-quota UI
    // lives on the admin user-detail page, keyed on the owning user).
    notify_user(&state, device.user_id, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ── Admin device lifecycle ──────────────────────────────────────────────
// Pause / resume / revoke any device without impersonating (or suspending)
// its owner — the moderation path for an abusive or compromised peer. The
// WG side effects are the same shared cores the owner's handlers use,
// so the tunnel stays correct either way; only the audit actor
// and action names differ.

#[utoipa::path(
    post,
    path = "/admin/devices/{id}/pause",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Device UUID")),
    responses(
        (status = 200, description = "Device paused; WG peer removed from the interface"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "No such device"),
        (status = 409, description = "Device is revoked"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_pause_device(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    admin_set_device_pause(&state, &actor, device_id, zerovpn_core::models::DeviceStatus::Paused)
        .await?;
    Ok(Json(json!({ "status": "paused" })))
}

#[utoipa::path(
    post,
    path = "/admin/devices/{id}/unpause",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Device UUID")),
    responses(
        (status = 200, description = "Device resumed; WG peer re-added to the interface"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "No such device"),
        (status = 409, description = "Device is revoked"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_unpause_device(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    admin_set_device_pause(&state, &actor, device_id, zerovpn_core::models::DeviceStatus::Active)
        .await?;
    Ok(Json(json!({ "status": "active" })))
}

async fn admin_set_device_pause(
    state: &AppState,
    actor: &zerovpn_core::models::User,
    device_id: Uuid,
    target: zerovpn_core::models::DeviceStatus,
) -> ApiResult<()> {
    use zerovpn_core::models::DeviceStatus;
    let device = devices::get_by_id(&state.pool, device_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    crate::routes::devices::apply_pause_state(state, &device, target).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: match target {
                DeviceStatus::Paused => "admin.device_paused",
                _ => "admin.device_unpaused",
            },
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({ "owner_user_id": device.user_id, "to": target }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, %device_id, ?target, "admin changed device pause state");
    crate::routes::devices::notify_device(
        state,
        device.user_id,
        Some(device_id),
        match target {
            DeviceStatus::Paused => ChangeAction::Paused,
            _ => ChangeAction::Unpaused,
        },
    );
    Ok(())
}

#[utoipa::path(
    delete,
    path = "/admin/devices/{id}",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Device UUID")),
    responses(
        (status = 200, description = "Device revoked; IP released and WG peer removed"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "No such device"),
        (status = 409, description = "Device already revoked"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn admin_revoke_device(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let device = devices::get_by_id(&state.pool, device_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if device.status == zerovpn_core::models::DeviceStatus::Revoked {
        return Err(ApiError::Conflict("device already revoked".into()));
    }
    crate::routes::devices::teardown_device(&state, &device).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.device_revoked",
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({ "owner_user_id": device.user_id }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, %device_id, owner = %device.user_id, "admin revoked device");
    crate::routes::devices::notify_device(
        &state,
        device.user_id,
        Some(device_id),
        ChangeAction::Deleted,
    );
    Ok(Json(json!({ "status": "ok" })))
}

#[utoipa::path(
    get,
    path = "/admin/failed-logins",
    tag = "Admin",
    params(AuditQuery),
    responses(
        (status = 200, description = "Recent failed login attempts", body = FailedLoginList),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_failed_logins(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    let (total,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::BIGINT FROM failed_logins")
            .fetch_one(&state.pool)
            .await?;
    let items: Vec<FailedLoginRow> = sqlx::query_as(
        r#"SELECT id,
                  email_attempted::TEXT AS email_attempted,
                  reason,
                  ip,
                  user_agent,
                  attempted_at
             FROM failed_logins
            ORDER BY id DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(FailedLoginList { total, items }))
}

// ---- Maintenance mode -----------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct MaintenanceState {
    pub maintenance_mode: bool,
    pub maintenance_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[utoipa::path(
    get,
    path = "/admin/maintenance",
    tag = "Admin",
    responses(
        (status = 200, description = "Current maintenance state", body = MaintenanceState),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_maintenance(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let row: MaintenanceState = sqlx::query_as(
        "SELECT maintenance_mode, maintenance_message, updated_at FROM app_settings WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(row))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MaintenanceBody {
    pub maintenance_mode: bool,
    pub maintenance_message: Option<String>,
}

#[utoipa::path(
    put,
    path = "/admin/maintenance",
    tag = "Admin",
    request_body = MaintenanceBody,
    responses(
        (status = 200, description = "Maintenance toggled (writes blocked for non-admins while ON)", body = StatusAck),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_maintenance(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Json(body): Json<MaintenanceBody>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "UPDATE app_settings SET maintenance_mode = $1, maintenance_message = $2 WHERE id = 1",
    )
    .bind(body.maintenance_mode)
    .bind(&body.maintenance_message)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.maintenance_mode",
            target_type: None,
            target_id: None,
            metadata: json!({ "on": body.maintenance_mode }),
            ip: None,
        },
    )
    .await?;
    info!(on = body.maintenance_mode, "maintenance mode toggled");
    // Admin-global: every admin console + the maintenance banner refreshes.
    notify_admin(&state, ResourceKind::Maintenance, None, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// ---- Non-admin user policy ------------------------------------------------
//
// Small set of admin-set toggles that govern what *non-admin* users see in the
// user-facing app. Stored on the existing `app_settings` singleton next to
// maintenance mode. Surfaced to every session via the `user_policy` field on
// the `/me` response; the frontend uses that to gate routes/links and the
// admin Users page reads/writes them here.

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct UserPolicy {
    /// When ON, the user app hides "View details" links on its device cards
    /// and bounces non-admin navigations to /app/devices/{id} back to the
    /// device list. Admins are exempt.
    pub hide_device_detail: bool,
}

#[utoipa::path(
    get,
    path = "/admin/user-policy",
    tag = "Admin",
    responses(
        (status = 200, description = "Current global non-admin user policy", body = UserPolicy),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_user_policy(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let row: UserPolicy = sqlx::query_as(
        "SELECT policy_hide_device_detail AS hide_device_detail
           FROM app_settings WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(row))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UserPolicyBody {
    pub hide_device_detail: bool,
}

#[utoipa::path(
    put,
    path = "/admin/user-policy",
    tag = "Admin",
    request_body = UserPolicyBody,
    responses(
        (status = 200, description = "Policy updated; live sessions see the change on their next /me", body = StatusAck),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_user_policy(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Json(body): Json<UserPolicyBody>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "UPDATE app_settings SET policy_hide_device_detail = $1 WHERE id = 1",
    )
    .bind(body.hide_device_detail)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.user_policy",
            target_type: None,
            target_id: None,
            metadata: json!({ "hide_device_detail": body.hide_device_detail }),
            ip: None,
        },
    )
    .await?;
    info!(
        hide_device_detail = body.hide_device_detail,
        "non-admin user policy updated"
    );
    notify_admin(&state, ResourceKind::Maintenance, None, ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

// --- Server admin -----------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminServer {
    pub id: Uuid,
    pub name: String,
    pub region: String,
    pub endpoint_host: String,
    pub endpoint_port: i32,
    pub public_key: String,
    /// CIDR rendered as a string ("10.0.0.0/22").
    pub cidr: String,
    pub mtu: i32,
    pub is_active: bool,
    /// WireGuard PersistentKeepalive (seconds) for peers on this server.
    /// `0` disables keepalive.
    pub persistent_keepalive: i32,
    /// Cumulative lifetime RX/TX across this server's (non-revoked) devices —
    /// the accurate per-device lifetime sum (not the drift-prone aggregates).
    /// Merged in by `list_servers`; `0` on responses that don't compute it.
    pub rx_total: i64,
    pub tx_total: i64,
}

impl From<Server> for AdminServer {
    fn from(s: Server) -> Self {
        Self {
            id: s.id,
            name: s.name,
            region: s.region,
            endpoint_host: s.endpoint_host,
            endpoint_port: s.endpoint_port,
            public_key: s.public_key,
            cidr: s.cidr.to_string(),
            mtu: s.mtu,
            is_active: s.is_active,
            persistent_keepalive: s.persistent_keepalive as i32,
            // Totals are merged in by handlers that compute them (list_servers).
            rx_total: 0,
            tx_total: 0,
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/servers",
    tag = "Admin",
    responses(
        (status = 200, description = "Active WG servers", body = Vec<AdminServer>),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_servers(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let rows = servers::list_active(&state.pool).await?;
    // Merge in each server's cumulative lifetime RX/TX (accurate device-totals
    // source) for the server-live card.
    let totals: std::collections::HashMap<Uuid, (i64, i64)> =
        devices::server_lifetime_totals(&state.pool)
            .await?
            .into_iter()
            .map(|(id, rx, tx)| (id, (rx, tx)))
            .collect();
    let out: Vec<AdminServer> = rows
        .into_iter()
        .map(|s| {
            let mut a: AdminServer = s.into();
            if let Some((rx, tx)) = totals.get(&a.id) {
                a.rx_total = *rx;
                a.tx_total = *tx;
            }
            a
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchServerBody {
    pub name: Option<String>,
    pub endpoint_host: Option<String>,
    pub endpoint_port: Option<i32>,
    pub mtu: Option<i32>,
    /// WireGuard `PersistentKeepalive` (seconds). `0` disables. Bounded to
    /// match the DB CHECK constraint (`0..=3600`).
    pub persistent_keepalive: Option<i32>,
}

#[utoipa::path(
    patch,
    path = "/admin/servers/{id}",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Server UUID"),
    ),
    request_body = PatchServerBody,
    responses(
        (status = 200, description = "Server config updated", body = StatusAck),
        (status = 400, description = "Validation error (port / MTU)"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn patch_server(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchServerBody>,
) -> ApiResult<impl IntoResponse> {
    if let Some(name) = &body.name
        && name.trim().is_empty() {
            return Err(ApiError::Validation("server name cannot be empty".into()));
        }
    if let Some(port) = body.endpoint_port
        && !(1..=65535).contains(&port) {
            return Err(ApiError::Validation("endpoint_port must be 1..=65535".into()));
        }
    if let Some(mtu) = body.mtu
        && !(576..=9000).contains(&mtu) {
            return Err(ApiError::Validation("mtu must be 576..=9000".into()));
        }
    if let Some(ka) = body.persistent_keepalive
        && !(0..=3600).contains(&ka) {
            return Err(ApiError::Validation(
                "persistent_keepalive must be 0..=3600 (0 disables)".into(),
            ));
        }
    sqlx::query(
        r#"UPDATE servers
           SET name                 = COALESCE($2, name),
               endpoint_host        = COALESCE($3, endpoint_host),
               endpoint_port        = COALESCE($4, endpoint_port),
               mtu                  = COALESCE($5, mtu),
               persistent_keepalive = COALESCE($6, persistent_keepalive)
           WHERE id = $1"#,
    )
    .bind(id)
    .bind(&body.name)
    .bind(&body.endpoint_host)
    .bind(body.endpoint_port)
    .bind(body.mtu)
    .bind(body.persistent_keepalive.map(|v| v as i16))
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.server_patched",
            target_type: Some("server"),
            target_id: Some(id),
            metadata: json!({
                "name": body.name,
                "endpoint_host": body.endpoint_host,
                "endpoint_port": body.endpoint_port,
                "mtu": body.mtu,
                "persistent_keepalive": body.persistent_keepalive,
            }),
            ip: None,
        },
    )
    .await?;
    info!(actor = %actor.id, server = %id, "server patched");
    notify_admin(&state, ResourceKind::Server, Some(id), ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RotateServerAck {
    #[schema(example = "ok")]
    pub status: &'static str,
    pub new_public_key: String,
    pub wg0_conf_rewritten: bool,
    pub warning: String,
}

#[utoipa::path(
    post,
    path = "/admin/servers/{id}/rotate-keys",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Server UUID"),
    ),
    responses(
        (status = 200, description = "Keypair rotated; every peer .conf is now stale", body = RotateServerAck),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Server not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn rotate_server_keys(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let server = servers::find_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let private = zerovpn_wg::keys::generate_private_key();
    let public = zerovpn_wg::keys::derive_public_key(&private)
        .map_err(|e| ApiError::Internal(format!("derive public key: {e}")))?;
    let encrypted = state
        .kek
        .encrypt(private.as_bytes())
        .map_err(|e| ApiError::Internal(format!("encrypt server key: {e}")))?;

    // 1. Persist BOTH halves in the DB — it is the source of truth for the
    //    server key (the api restores wg0.conf from it on boot).
    servers::set_keypair(&state.pool, id, &public, &encrypted).await?;

    // 2. Rewrite wg0.conf (no PostUp — the bring-up applies iptables) and
    //    re-apply it so the new key takes effect on the live interface. The api
    //    is the WG host now, so no external container restart is needed.
    let conf_path = std::env::var("ZEROVPN_WG__SERVER_CONFIG_PATH")
        .unwrap_or_else(|_| "/wg/wg0.conf".to_string());
    let listen_port = server.endpoint_port;
    let server_address = format!("{}/{}", server.cidr.network(), server.cidr.prefix());
    let conf = format!(
        "# Auto-generated by zerovpn-api after key rotation.\n\
         [Interface]\n\
         PrivateKey = {private}\n\
         Address = {server_address}\n\
         ListenPort = {listen_port}\n\
         SaveConfig = false\n",
    );
    let conf_write_ok = match tokio::fs::write(&conf_path, conf.as_bytes()).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(?e, path = %conf_path, "wg0.conf rewrite failed during key rotation");
            false
        }
    };
    crate::bootstrap::reapply_wg_interface(&state.pool).await;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.server_keys_rotated",
            target_type: Some("server"),
            target_id: Some(id),
            metadata: json!({
                "new_public_key": public,
                "wg0_conf_rewritten": conf_write_ok,
            }),
            ip: None,
        },
    )
    .await?;

    info!(actor = %actor.id, server = %id, "server keys rotated");
    notify_admin(&state, ResourceKind::Server, Some(id), ChangeAction::KeysRotated);
    Ok(Json(json!({
        "status": "ok",
        "new_public_key": public,
        "wg0_conf_rewritten": conf_write_ok,
        "warning": "All peer .conf files reference the OLD server pubkey and must be re-downloaded. Restart the wg container to pick up the new private key.",
    })))
}

// ---- Server detail -------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminServerDeviceRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_email: String,
    pub name: String,
    pub os: zerovpn_core::models::DeviceOs,
    pub status: zerovpn_core::models::DeviceStatus,
    pub allocated_ip: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_handshake_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminServerDetail {
    pub server: AdminServer,
    pub device_count_active: i64,
    pub device_count_paused: i64,
    pub device_count_total: i64,
    pub devices: Vec<AdminServerDeviceRow>,
}

/// Bundled server detail for the admin server-detail page. Server core
/// + per-status device counts + the list of devices on this server
/// joined with each device's owning user email.
#[utoipa::path(
    get,
    path = "/admin/servers/{id}",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target server UUID")),
    responses(
        (status = 200, description = "Server detail bundle", body = AdminServerDetail),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Server not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn server_detail(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let server = servers::find_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // One inline query joins devices to users so we can show emails
    // next to each device row without N round trips.
    let device_rows: Vec<AdminServerDeviceRow> = sqlx::query_as::<_, (
        Uuid,
        Uuid,
        String,
        String,
        zerovpn_core::models::DeviceOs,
        zerovpn_core::models::DeviceStatus,
        ipnetwork::IpNetwork,
        Option<OffsetDateTime>,
        OffsetDateTime,
    )>(
        r#"SELECT d.id, d.user_id, u.email::TEXT, d.name, d.os, d.status,
                  d.allocated_ip, d.last_handshake_at, d.created_at
             FROM devices d
             JOIN users u ON u.id = d.user_id
            WHERE d.server_id = $1 AND d.status <> 'revoked'
            ORDER BY d.created_at DESC
            LIMIT 200"#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|r| AdminServerDeviceRow {
        id: r.0,
        user_id: r.1,
        user_email: r.2,
        name: r.3,
        os: r.4,
        status: r.5,
        allocated_ip: r.6.ip().to_string(),
        last_handshake_at: r.7,
        created_at: r.8,
    })
    .collect();

    // Per-status counts via a single CASE-aggregating query — cheaper
    // than two round trips and consistent with the user-detail row.
    let counts: (i64, i64, i64) = sqlx::query_as(
        r#"SELECT
              COUNT(*) FILTER (WHERE status = 'active')::BIGINT  AS active,
              COUNT(*) FILTER (WHERE status = 'paused')::BIGINT  AS paused,
              COUNT(*) FILTER (WHERE status <> 'revoked')::BIGINT AS total
             FROM devices WHERE server_id = $1"#,
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(AdminServerDetail {
        server: AdminServer::from(server),
        device_count_active: counts.0,
        device_count_paused: counts.1,
        device_count_total: counts.2,
        devices: device_rows,
    }))
}

#[utoipa::path(
    get,
    path = "/admin/servers/{id}/bandwidth",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Target server UUID"),
        BandwidthRangeQuery,
    ),
    responses(
        (status = 200, description = "Bucketed RX/TX history aggregated across the server's devices", body = AdminUserBandwidthResponse),
        (status = 400, description = "Invalid range"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn server_bandwidth(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<Uuid>,
    Query(q): Query<BandwidthRangeQuery>,
) -> ApiResult<impl IntoResponse> {
    let range = q.range.unwrap_or_else(|| "24h".into());
    let (since, bucket) = match range.as_str() {
        "24h" => (OffsetDateTime::now_utc() - time::Duration::hours(24), "hour"),
        "7d" => (OffsetDateTime::now_utc() - time::Duration::days(7), "hour"),
        "30d" => (OffsetDateTime::now_utc() - time::Duration::days(30), "day"),
        other => {
            return Err(ApiError::Validation(format!(
                "range must be 24h | 7d | 30d (got {other})"
            )));
        }
    };
    let rows = bandwidth::server_totals(&state.pool, id, since, bucket).await?;
    Ok(Json(AdminUserBandwidthResponse {
        bucket: if bucket == "hour" { "hour" } else { "day" },
        range,
        buckets: rows
            .into_iter()
            .map(|b| AdminUserBandwidthBucket {
                bucket_start: b.bucket_start,
                rx_bytes: b.rx_bytes,
                tx_bytes: b.tx_bytes,
            })
            .collect(),
    }))
}

/// Admin-only: every non-revoked device across the deployment, used to
/// render the fleet-wide topology view. Each row carries its owning
/// `user_id` so the frontend can cluster devices under their user node.
#[utoipa::path(
    get,
    path = "/admin/devices",
    tag = "Admin",
    responses(
        (status = 200, description = "All non-revoked devices (every user)", body = Vec<PublicDevice>),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_devices(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let rows = devices::list_all_active(&state.pool).await?;
    let out: Vec<PublicDevice> = rows.into_iter().map(Into::into).collect();
    Ok(Json(out))
}

/// Admin-only: WireGuard peer-endpoint history for a single device.
/// Each row is one observation of a distinct `host:port` the device
/// connected from, newest first. Capped at 200 rows.
#[utoipa::path(
    get,
    path = "/admin/devices/{id}/endpoint-history",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Distinct WG peer endpoints for the device, newest first", body = Vec<peer_endpoint_history::EndpointRow>),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn device_endpoint_history(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let rows = peer_endpoint_history::list_for_device(&state.pool, device_id, 200).await?;
    Ok(Json(rows))
}

/// Admin-only: per-device connection-session log. One row per
/// WireGuard connection (online → offline pair), with start / end
/// endpoints and rx/tx byte counters so admins can read each session's
/// duration and traffic without joining over transition pairs.
/// Open sessions (still online) come back with `ended_at = null`.
#[utoipa::path(
    get,
    path = "/admin/devices/{id}/connection-history",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Device's connection sessions, newest first", body = Vec<connection_sessions::ConnectionSessionRow>),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn device_connection_history(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let rows = connection_sessions::list_for_device(&state.pool, device_id, 200).await?;
    Ok(Json(rows))
}

// ---- Admin device detail -------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceOwner {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceDetail {
    pub id: Uuid,
    pub user_id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub os: zerovpn_core::models::DeviceOs,
    pub device_type: zerovpn_core::models::DeviceType,
    pub status: zerovpn_core::models::DeviceStatus,
    pub allocated_ip: String,
    pub public_key: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_handshake_at: Option<OffsetDateTime>,
    pub last_peer_endpoint: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_peer_endpoint_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Authoritative lifetime totals (same source as the user-facing device
    /// card) so the admin view's RX/TX/Total KPIs match what the owner sees.
    pub total_rx_bytes: i64,
    pub total_tx_bytes: i64,
    /// Per-device monthly quota (cap + usage + auto-pause flag) for the
    /// Quota KPI — mirrors the user device page.
    pub monthly_byte_cap: Option<i64>,
    pub current_month_bytes: i64,
    pub auto_paused: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceActivity {
    pub id: i64,
    pub action: String,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceDetailResponse {
    pub device: AdminDeviceDetail,
    pub owner: AdminDeviceOwner,
    /// Recent audit entries where this device is the *target*. Newest
    /// first, hard-capped at 50. Endpoint history and connection
    /// sessions live behind their own paginated endpoints
    /// (`/admin/devices/{id}/endpoint-history` and `/connection-history`).
    pub activity: Vec<AdminDeviceActivity>,
}

/// Bundled admin device detail. Returns the device core, its owner,
/// and the most recent 50 audit entries targeting the device. The
/// frontend pulls endpoint + connection history in parallel.
#[utoipa::path(
    get,
    path = "/admin/devices/{id}",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Device UUID")),
    responses(
        (status = 200, description = "Bundled admin device detail", body = AdminDeviceDetailResponse),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Device not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn device_detail(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(device_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Direct SELECT so we can fetch the Stage A peer-endpoint columns
    // without touching the core `Device` model.
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        server_id: Uuid,
        name: String,
        os: zerovpn_core::models::DeviceOs,
        device_type: zerovpn_core::models::DeviceType,
        status: zerovpn_core::models::DeviceStatus,
        allocated_ip: ipnetwork::IpNetwork,
        public_key: String,
        last_handshake_at: Option<OffsetDateTime>,
        last_peer_endpoint: Option<String>,
        last_peer_endpoint_at: Option<OffsetDateTime>,
        created_at: OffsetDateTime,
        lifetime_rx_bytes: i64,
        lifetime_tx_bytes: i64,
        monthly_byte_cap: Option<i64>,
        current_month_bytes: i64,
        auto_paused: bool,
    }
    let row: Row = sqlx::query_as(
        r#"SELECT id, user_id, server_id, name, os, device_type, status, allocated_ip,
                  public_key, last_handshake_at,
                  last_peer_endpoint, last_peer_endpoint_at, created_at,
                  lifetime_rx_bytes, lifetime_tx_bytes,
                  monthly_byte_cap, current_month_bytes, auto_paused
             FROM devices
            WHERE id = $1"#,
    )
    .bind(device_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;

    let owner_row = users::find_by_id(&state.pool, row.user_id)
        .await?
        .ok_or_else(|| ApiError::Internal("device owner missing".into()))?;

    let activity_rows = audit::list_for_target(&state.pool, "device", device_id, 50).await?;
    let activity = activity_rows
        .into_iter()
        .map(|a| AdminDeviceActivity {
            id: a.id,
            action: a.action,
            metadata: a.metadata,
            created_at: a.created_at,
        })
        .collect();

    Ok(Json(AdminDeviceDetailResponse {
        device: AdminDeviceDetail {
            id: row.id,
            user_id: row.user_id,
            server_id: row.server_id,
            name: row.name,
            os: row.os,
            device_type: row.device_type,
            status: row.status,
            allocated_ip: row.allocated_ip.ip().to_string(),
            public_key: row.public_key,
            last_handshake_at: row.last_handshake_at,
            last_peer_endpoint: row.last_peer_endpoint,
            last_peer_endpoint_at: row.last_peer_endpoint_at,
            created_at: row.created_at,
            total_rx_bytes: row.lifetime_rx_bytes,
            total_tx_bytes: row.lifetime_tx_bytes,
            monthly_byte_cap: row.monthly_byte_cap,
            current_month_bytes: row.current_month_bytes,
            auto_paused: row.auto_paused,
        },
        owner: AdminDeviceOwner {
            id: owner_row.id,
            email: owner_row.email,
            role: owner_row.role,
            status: owner_row.status,
        },
        activity,
    }))
}

// ---- Admin device bandwidth ----------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceBandwidthBucket {
    #[serde(with = "time::serde::rfc3339")]
    pub bucket_start: OffsetDateTime,
    pub rx_bytes: i64,
    pub tx_bytes: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminDeviceBandwidthResponse {
    pub bucket: &'static str,
    pub range: String,
    pub buckets: Vec<AdminDeviceBandwidthBucket>,
}

#[utoipa::path(
    get,
    path = "/admin/devices/{id}/bandwidth",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Device UUID"), BandwidthRangeQuery),
    responses(
        (status = 200, description = "Bucketed RX/TX history for this device", body = AdminDeviceBandwidthResponse),
        (status = 400, description = "Invalid range"),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn device_bandwidth(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(device_id): Path<Uuid>,
    Query(q): Query<BandwidthRangeQuery>,
) -> ApiResult<impl IntoResponse> {
    let range = q.range.unwrap_or_else(|| "24h".into());
    let (rows, bucket) = match range.as_str() {
        "24h" => (bandwidth::device_hourly(&state.pool, device_id, 24).await?, "hour"),
        "7d" => (bandwidth::device_hourly(&state.pool, device_id, 24 * 7).await?, "hour"),
        "30d" => (bandwidth::device_daily(&state.pool, device_id, 30).await?, "day"),
        other => {
            return Err(ApiError::Validation(format!(
                "range must be 24h | 7d | 30d (got {other})"
            )));
        }
    };
    Ok(Json(AdminDeviceBandwidthResponse {
        bucket: if bucket == "hour" { "hour" } else { "day" },
        range,
        buckets: rows
            .into_iter()
            .map(|b| AdminDeviceBandwidthBucket {
                bucket_start: b.bucket_start,
                rx_bytes: b.rx_bytes,
                tx_bytes: b.tx_bytes,
            })
            .collect(),
    }))
}

// ---- Session events ------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct SessionEventsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Filter by user UUID. Omit to include every user.
    #[serde(default)]
    pub user_id: Option<Uuid>,
    /// Filter by event kind (login, logout, etc).
    #[serde(default)]
    pub event: Option<session_events::SessionEvent>,
    /// Filter by IP (accepts the full `/32`/`/128` host form or a bare
    /// address — Postgres widens either way).
    #[serde(default)]
    pub ip: Option<String>,
    /// Inclusive lower bound on `created_at`.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    /// Exclusive upper bound on `created_at`.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionEventList {
    pub total: i64,
    pub items: Vec<session_events::SessionEventRow>,
}

#[utoipa::path(
    get,
    path = "/admin/session-events",
    tag = "Admin",
    params(SessionEventsQuery),
    responses(
        (status = 200, description = "Account-security event feed, newest first", body = SessionEventList),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_session_events(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Query(q): Query<SessionEventsQuery>,
) -> ApiResult<impl IntoResponse> {
    let f = session_events::Filters {
        user_id: q.user_id,
        event: q.event,
        ip: q.ip.as_deref(),
        since: q.since,
        until: q.until,
    };
    let total = session_events::count_recent(&state.pool, f).await?;
    let items = session_events::list_recent(&state.pool, f, q.limit, q.offset).await?;
    Ok(Json(SessionEventList { total, items }))
}

// ---- Access logs --------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct AccessLogsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Filter by user UUID. Omit to include unauthenticated rows too.
    #[serde(default)]
    pub user_id: Option<Uuid>,
    /// Exact HTTP method (GET / POST / PUT / PATCH / DELETE).
    #[serde(default)]
    pub method: Option<String>,
    /// Path prefix. `/api/v1/admin` matches every admin endpoint.
    #[serde(default)]
    pub path: Option<String>,
    /// Lower bound on the HTTP status code (inclusive). Combined with
    /// `status_max` this lets admins surface "every 4xx in the last
    /// hour" or "every 5xx today".
    #[serde(default)]
    pub status_min: Option<i16>,
    #[serde(default)]
    pub status_max: Option<i16>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AccessLogList {
    pub total: i64,
    pub items: Vec<access_logs::AccessLogRow>,
}

#[utoipa::path(
    get,
    path = "/admin/access-logs",
    tag = "Admin",
    params(AccessLogsQuery),
    responses(
        (status = 200, description = "Per-request HTTP access log, newest first", body = AccessLogList),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_access_logs(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    headers: axum::http::HeaderMap,
    Query(q): Query<AccessLogsQuery>,
) -> ApiResult<impl IntoResponse> {
    let _ = zerovpn_db::repos::audit::record_with_ua(
        &state.pool,
        zerovpn_db::repos::audit::AuditEntry {
            action: "admin_viewed_logs",
            actor_user_id: Some(admin.id),
            target_type: Some("system"),
            target_id: None,
            metadata: serde_json::json!({"path": "list_access_logs"}),
ip: crate::routes::auth::client_ip(&headers),
        },
        crate::routes::auth::client_user_agent(&headers).as_deref(),
    )
    .await;

    let f = access_logs::Filters {
        user_id: q.user_id,
        method: q.method.as_deref(),
        path_prefix: q.path.as_deref(),
        status_min: q.status_min,
        status_max: q.status_max,
        ip: q.ip.as_deref(),
        since: q.since,
        until: q.until,
    };
    let total = access_logs::count_recent(&state.pool, f).await?;
    let items = access_logs::list_recent(&state.pool, f, q.limit, q.offset).await?;
    Ok(Json(AccessLogList { total, items }))
}

// ---- Finder (Phase 2 / Stage B) ------------------------------------------
//
// Cross-source admin search. Given a free-form query the endpoint
// detects the most likely shape (IPv4/IPv6 host address, `host:port`
// WG endpoint, or freetext) and runs targeted COUNT queries against
// every log table that could match plus a small list of direct
// user/device matches. The frontend renders the counts as
// click-through cards that deep-link into the existing filtered
// admin pages.

#[derive(Debug, Deserialize, IntoParams)]
pub struct FinderQuery {
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FinderUserMatch {
    pub id: Uuid,
    pub email: String,
    /// `"email"` (matched the email substring) or `"last_login_ip"`
    /// (their `users.last_login_ip` equals the query).
    pub matched_on: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FinderDeviceMatch {
    pub id: Uuid,
    pub user_id: Uuid,
    /// Owner's email so the finder can say who a matched device belongs
    /// to without a second lookup — the UI groups device matches by owner.
    pub user_email: String,
    pub name: String,
    pub allocated_ip: String,
    pub last_peer_endpoint: Option<String>,
    /// `"name"` (substring), `"allocated_ip"` (exact), or
    /// `"last_peer_endpoint"` (`host:port` prefix).
    pub matched_on: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FinderCounts {
    pub audit_logs: i64,
    pub failed_logins: i64,
    pub session_events: i64,
    pub access_logs: i64,
    /// `peer_endpoint_history` rows whose endpoint starts with the
    /// query (so admins can pivot from "this IP" to "every device that
    /// ever connected from that IP").
    pub peer_endpoint_history: i64,
    /// `connection_sessions` matching the query on either start- or
    /// end-side endpoint.
    pub connection_sessions: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FinderResponse {
    /// Echoed back so the page can render "Results for X" without
    /// trusting the URL bar.
    pub query: String,
    /// Detected query kind. `"ip"` for a bare host address, `"endpoint"`
    /// for `host:port`, `"regex"` for a `/pattern/` POSIX regex,
    /// `"text"` otherwise. The frontend uses this to decide which
    /// result groups to show prominently.
    pub kind: &'static str,
    pub counts: FinderCounts,
    pub users: Vec<FinderUserMatch>,
    pub devices: Vec<FinderDeviceMatch>,
}

fn detect_kind(q: &str) -> &'static str {
    // Regex: `/pattern/`. Reject the empty-body form so plain `//`
    // doesn't get parsed as a wildcard regex.
    if q.len() >= 3 && q.starts_with('/') && q.ends_with('/') {
        return "regex";
    }
    // `host:port` — bracketed v6 or `1.2.3.4:51820`. Heuristic: ends
    // with `:digits` and the front parses as an address.
    if let Some(colon) = q.rfind(':') {
        let (host, port) = q.split_at(colon);
        let port = &port[1..];
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            if host.parse::<std::net::IpAddr>().is_ok() {
                return "endpoint";
            }
        }
    }
    if q.parse::<std::net::IpAddr>().is_ok() {
        return "ip";
    }
    "text"
}

/// Cap regex length to bound the search the database has to do. POSIX
/// regex on Postgres uses a backtracking engine, so a maliciously
/// crafted pattern can pin a CPU; 200 chars is plenty for legitimate
/// admin queries and keeps the worst case bounded.
const FINDER_REGEX_MAX_LEN: usize = 200;

#[utoipa::path(
    get,
    path = "/admin/finder",
    tag = "Admin",
    params(FinderQuery),
    responses(
        (status = 200, description = "Cross-source admin search results", body = FinderResponse),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn finder(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    headers: axum::http::HeaderMap,
    Query(query): Query<FinderQuery>,
) -> ApiResult<impl IntoResponse> {
    let _ = zerovpn_db::repos::audit::record_with_ua(
        &state.pool,
        zerovpn_db::repos::audit::AuditEntry {
            action: "admin_viewed_logs",
            actor_user_id: Some(admin.id),
            target_type: Some("system"),
            target_id: None,
            metadata: serde_json::json!({"path": "finder"}),
ip: crate::routes::auth::client_ip(&headers),
        },
        crate::routes::auth::client_user_agent(&headers).as_deref(),
    )
    .await;

    let q = query.q.unwrap_or_default();
    let q_trim = q.trim().to_string();
    if q_trim.is_empty() {
        return Ok(Json(FinderResponse {
            query: q,
            kind: "text",
            counts: FinderCounts {
                audit_logs: 0,
                failed_logins: 0,
                session_events: 0,
                access_logs: 0,
                peer_endpoint_history: 0,
                connection_sessions: 0,
            },
            users: vec![],
            devices: vec![],
        }));
    }
    let kind = detect_kind(&q_trim);

    // Build the LIKE pattern for freetext substring matches once.
    let like_pat = format!("%{q_trim}%");
    // Endpoint prefix for `peer_endpoint_history.endpoint LIKE 'IP:%'`
    // when an admin pastes a bare IP (so we surface every device that
    // connected from that IP regardless of source port).
    let endpoint_prefix_pat = if kind == "ip" {
        Some(format!("{q_trim}:%"))
    } else {
        None
    };
    // INET cast — only meaningful when the input is a host address.
    let inet_q: Option<ipnetwork::IpNetwork> = if kind == "ip" {
        q_trim.parse::<std::net::IpAddr>().ok().map(Into::into)
    } else {
        None
    };
    // Regex body — the inner pattern between the leading and trailing
    // `/`. Validated here with Rust's `regex` crate (linear-time, fails
    // fast) before being handed to Postgres `~*` so an invalid pattern
    // becomes a 422 instead of a 500. Bounded length too — see
    // FINDER_REGEX_MAX_LEN.
    let regex_pat: Option<String> = if kind == "regex" {
        let body = &q_trim[1..q_trim.len() - 1];
        if body.is_empty() {
            return Err(ApiError::Validation("empty regex".into()));
        }
        if body.len() > FINDER_REGEX_MAX_LEN {
            return Err(ApiError::Validation(format!(
                "regex too long (max {FINDER_REGEX_MAX_LEN} chars)"
            )));
        }
        if let Err(e) = regex::Regex::new(body) {
            return Err(ApiError::Validation(format!("invalid regex: {e}")));
        }
        Some(body.to_string())
    } else {
        None
    };

    // ── Count queries ────────────────────────────────────────────────
    // Each counts rows that match the query in its most useful shape:
    //   ip       → exact IP match on the table's `ip` column
    //   endpoint → exact match on the table's endpoint column(s)
    //   text     → substring on `user_agent`
    let pool = &state.pool;

    let audit_count: i64 = match kind {
        "ip" => {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*)::BIGINT FROM audit_logs WHERE ip = $1")
                    .bind(inet_q)
                    .fetch_one(pool)
                    .await?;
            n
        }
        "text" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM audit_logs WHERE user_agent ILIKE $1",
            )
            .bind(&like_pat)
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM audit_logs
                  WHERE user_agent ~* $1
                     OR action      ~* $1
                     OR host(ip)    ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    let failed_logins_count: i64 = match kind {
        "ip" => {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*)::BIGINT FROM failed_logins WHERE ip = $1")
                    .bind(inet_q)
                    .fetch_one(pool)
                    .await?;
            n
        }
        "text" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM failed_logins WHERE user_agent ILIKE $1",
            )
            .bind(&like_pat)
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM failed_logins
                  WHERE user_agent             ~* $1
                     OR email_attempted::TEXT  ~* $1
                     OR host(ip)               ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    let session_events_count: i64 = match kind {
        "ip" => {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*)::BIGINT FROM session_events WHERE ip = $1")
                    .bind(inet_q)
                    .fetch_one(pool)
                    .await?;
            n
        }
        "text" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM session_events WHERE user_agent ILIKE $1",
            )
            .bind(&like_pat)
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM session_events
                  WHERE user_agent ~* $1
                     OR host(ip)    ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    let access_logs_count: i64 = match kind {
        "ip" => {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*)::BIGINT FROM access_logs WHERE ip = $1")
                    .bind(inet_q)
                    .fetch_one(pool)
                    .await?;
            n
        }
        "text" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM access_logs WHERE user_agent ILIKE $1",
            )
            .bind(&like_pat)
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM access_logs
                  WHERE user_agent ~* $1
                     OR path        ~* $1
                     OR host(ip)    ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    let peer_endpoint_history_count: i64 = match kind {
        "endpoint" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM peer_endpoint_history WHERE endpoint = $1",
            )
            .bind(&q_trim)
            .fetch_one(pool)
            .await?;
            n
        }
        "ip" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM peer_endpoint_history WHERE endpoint LIKE $1",
            )
            .bind(endpoint_prefix_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM peer_endpoint_history WHERE endpoint ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    let connection_sessions_count: i64 = match kind {
        "endpoint" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM connection_sessions
                  WHERE peer_endpoint_at_start = $1
                     OR peer_endpoint_at_end   = $1",
            )
            .bind(&q_trim)
            .fetch_one(pool)
            .await?;
            n
        }
        "ip" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM connection_sessions
                  WHERE peer_endpoint_at_start LIKE $1
                     OR peer_endpoint_at_end   LIKE $1",
            )
            .bind(endpoint_prefix_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        "regex" => {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT FROM connection_sessions
                  WHERE peer_endpoint_at_start ~* $1
                     OR peer_endpoint_at_end   ~* $1",
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_one(pool)
            .await?;
            n
        }
        _ => 0,
    };

    // ── Direct matches ───────────────────────────────────────────────
    let mut users: Vec<FinderUserMatch> = Vec::new();
    let mut devices: Vec<FinderDeviceMatch> = Vec::new();

    // User by last_login_ip (when ip-kind) or by email substring (text-kind).
    // Tuple-style query_as so the local row shape doesn't need a
    // `#[derive(FromRow)]` (which doesn't work in function scope).
    match kind {
        "ip" => {
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                r#"SELECT id, email::TEXT AS email FROM users
                    WHERE last_login_ip = $1
                    ORDER BY last_login_at DESC NULLS LAST
                    LIMIT 10"#,
            )
            .bind(inet_q)
            .fetch_all(pool)
            .await?;
            for (id, email) in rows {
                users.push(FinderUserMatch {
                    id,
                    email,
                    matched_on: "last_login_ip",
                });
            }
        }
        "text" => {
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                r#"SELECT id, email::TEXT AS email FROM users
                    WHERE email::TEXT ILIKE $1
                    ORDER BY created_at DESC
                    LIMIT 10"#,
            )
            .bind(&like_pat)
            .fetch_all(pool)
            .await?;
            for (id, email) in rows {
                users.push(FinderUserMatch {
                    id,
                    email,
                    matched_on: "email",
                });
            }
        }
        "regex" => {
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                r#"SELECT id, email::TEXT AS email FROM users
                    WHERE email::TEXT ~* $1
                    ORDER BY created_at DESC
                    LIMIT 10"#,
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_all(pool)
            .await?;
            for (id, email) in rows {
                users.push(FinderUserMatch {
                    id,
                    email,
                    matched_on: "email",
                });
            }
        }
        _ => {}
    }

    // Devices — same tuple-style shape:
    // (id, user_id, user_email, name, allocated_ip, last_peer_endpoint).
    {
        type DeviceTuple = (
            Uuid,
            Uuid,
            String,
            String,
            ipnetwork::IpNetwork,
            Option<String>,
        );
        let rows: Vec<DeviceTuple> = match kind {
            "ip" => {
                // allocated_ip exact match OR last_peer_endpoint starts
                // with `ip:`. Union both axes.
                sqlx::query_as(
                    r#"SELECT d.id, d.user_id, u.email::TEXT, d.name, d.allocated_ip, d.last_peer_endpoint
                         FROM devices d
                         JOIN users u ON u.id = d.user_id
                        WHERE d.status <> 'revoked'
                          AND (d.allocated_ip = $1
                               OR d.last_peer_endpoint LIKE $2)
                        ORDER BY d.created_at DESC
                        LIMIT 10"#,
                )
                .bind(inet_q)
                .bind(endpoint_prefix_pat.as_deref().unwrap_or(""))
                .fetch_all(pool)
                .await?
            }
            "endpoint" => sqlx::query_as(
                r#"SELECT d.id, d.user_id, u.email::TEXT, d.name, d.allocated_ip, d.last_peer_endpoint
                     FROM devices d
                     JOIN users u ON u.id = d.user_id
                    WHERE d.status <> 'revoked' AND d.last_peer_endpoint = $1
                    ORDER BY d.created_at DESC
                    LIMIT 10"#,
            )
            .bind(&q_trim)
            .fetch_all(pool)
            .await?,
            "regex" => sqlx::query_as(
                r#"SELECT d.id, d.user_id, u.email::TEXT, d.name, d.allocated_ip, d.last_peer_endpoint
                     FROM devices d
                     JOIN users u ON u.id = d.user_id
                    WHERE d.status <> 'revoked'
                      AND (d.name ~* $1 OR d.last_peer_endpoint ~* $1)
                    ORDER BY d.created_at DESC
                    LIMIT 10"#,
            )
            .bind(regex_pat.as_deref().unwrap_or(""))
            .fetch_all(pool)
            .await?,
            _ => sqlx::query_as(
                r#"SELECT d.id, d.user_id, u.email::TEXT, d.name, d.allocated_ip, d.last_peer_endpoint
                     FROM devices d
                     JOIN users u ON u.id = d.user_id
                    WHERE d.status <> 'revoked' AND d.name ILIKE $1
                    ORDER BY d.created_at DESC
                    LIMIT 10"#,
            )
            .bind(&like_pat)
            .fetch_all(pool)
            .await?,
        };
        let bare_ip = q_trim.clone();
        let regex_body = regex_pat.clone();
        for (id, user_id, user_email, name, allocated_ip, last_peer_endpoint) in rows {
            let matched_on: &'static str = match kind {
                "endpoint" => "last_peer_endpoint",
                "ip" => {
                    if last_peer_endpoint
                        .as_deref()
                        .is_some_and(|e| e.starts_with(&format!("{bare_ip}:")))
                    {
                        "last_peer_endpoint"
                    } else {
                        "allocated_ip"
                    }
                }
                "regex" => {
                    // Re-evaluate which axis matched so the UI can show
                    // "last_peer_endpoint" vs "name". The pattern is
                    // already known-valid (we compiled it above).
                    let re = regex_body
                        .as_deref()
                        .and_then(|p| regex::Regex::new(&format!("(?i){p}")).ok());
                    let ep_hit = re.as_ref().is_some_and(|re| {
                        last_peer_endpoint
                            .as_deref()
                            .is_some_and(|e| re.is_match(e))
                    });
                    if ep_hit { "last_peer_endpoint" } else { "name" }
                }
                _ => "name",
            };
            devices.push(FinderDeviceMatch {
                id,
                user_id,
                user_email,
                name,
                allocated_ip: allocated_ip.ip().to_string(),
                last_peer_endpoint,
                matched_on,
            });
        }
    }

    Ok(Json(FinderResponse {
        query: q,
        kind,
        counts: FinderCounts {
            audit_logs: audit_count,
            failed_logins: failed_logins_count,
            session_events: session_events_count,
            access_logs: access_logs_count,
            peer_endpoint_history: peer_endpoint_history_count,
            connection_sessions: connection_sessions_count,
        },
        users,
        devices,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminStatsResponse {
    pub total: i64,
    pub active: i64,
    pub suspended: i64,
    pub pending_verification: i64,
    pub devices_total: i64,
    /// Devices online right now (active + handshake within 180s) — matches
    /// the user dashboard's definition, DB-backed so it's correct on load.
    pub online_now: i64,
}

/// Deployment-wide user + device counts. Powers the admin overview KPI
/// strip; replaces the previous client-side counts which were bounded to
/// whatever page of the user list the admin was looking at.
#[utoipa::path(
    get,
    path = "/admin/stats",
    tag = "Admin",
    responses(
        (status = 200, description = "Aggregate user + device counts", body = AdminStatsResponse),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn stats(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let s = users::admin_stats(&state.pool).await?;
    Ok(Json(AdminStatsResponse {
        total: s.total,
        active: s.active,
        suspended: s.suspended,
        pending_verification: s.pending_verification,
        devices_total: s.devices_total,
        online_now: s.online_now,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminFleetBandwidthResponse {
    /// All-time bytes received by every user's devices.
    pub rx_bytes: i64,
    /// All-time bytes transmitted by every user's devices.
    pub tx_bytes: i64,
}

/// Fleet-wide all-time bandwidth. Pulled from the hourly
/// `bandwidth_aggregates` rollups so the value survives reloads and
/// doesn't depend on a live WS connection.
#[utoipa::path(
    get,
    path = "/admin/bandwidth",
    tag = "Admin",
    responses(
        (status = 200, description = "Fleet RX/TX totals", body = AdminFleetBandwidthResponse),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn fleet_bandwidth(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    let (rx_bytes, tx_bytes) = bandwidth::fleet_totals(&state.pool).await?;
    Ok(Json(AdminFleetBandwidthResponse { rx_bytes, tx_bytes }))
}

// --- Impersonation ---------------------------------------------------------

/// Begin impersonating a user. Swaps the session's `user_id` to the target
/// while saving the admin's real identity under `real_user_id` so it can be
/// restored when impersonation ends. Requires admin role.
#[utoipa::path(
    post,
    path = "/admin/users/{id}/impersonate",
    tag = "Admin",
    params(("id" = Uuid, Path, description = "Target user UUID")),
    responses(
        (status = 200, description = "Admin session now impersonates the target user", body = StatusAck),
        (status = 400, description = "Cannot impersonate yourself"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn impersonate_user(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    session: Session,
    headers: HeaderMap,
    Path(target_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    if target_id == actor.id {
        return Err(ApiError::Validation("cannot impersonate yourself".into()));
    }

    let target = users::find_by_id(&state.pool, target_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if target.status != UserStatus::Active {
        return Err(ApiError::Validation(
            "can only impersonate active users".into(),
        ));
    }

    // Preserve the admin's real identity so we can restore it later.
    session
        .insert(SESSION_KEY_REAL_USER_ID, actor.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(
            SESSION_KEY_REAL_PW_CHANGED_AT,
            actor.password_changed_at.unix_timestamp(),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Swap to the target user.
    session
        .insert(SESSION_KEY_USER_ID, target.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(
            SESSION_KEY_PW_CHANGED_AT,
            target.password_changed_at.unix_timestamp(),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(actor.id),
            action: "admin.impersonate_start",
            target_type: Some("user"),
            target_id: Some(target_id),
            metadata: json!({ "target_email": target.email }),
            ip: None,
        },
    )
    .await?;
    // Phase 2 / Stage B — record under the TARGET user's id so the
    // per-user Sessions panel surfaces "you were impersonated by $admin".
    // The cross-fleet admin Sessions page picks it up via event filter.
    if let Err(e) = session_events::record(
        &state.pool,
        target_id,
        session_events::SessionEvent::ImpersonationStart,
        crate::routes::auth::client_ip(&headers),
        crate::routes::auth::client_user_agent(&headers).as_deref(),
        json!({ "by_user_id": actor.id, "by_email": actor.email }),
    )
    .await
    {
        warn!(?e, target = %target_id, "session_events impersonation_start record failed");
    }
    info!(actor = %actor.id, target = %target_id, "admin started impersonation");

    Ok(Json(json!({ "status": "ok" })))
}

/// Stop impersonating. Restores the admin's real session identity.
/// Does not require `RequireAdmin` because the active session now belongs
/// to the impersonated (possibly non-admin) user.
#[utoipa::path(
    post,
    path = "/admin/impersonate/stop",
    tag = "Admin",
    responses(
        (status = 200, description = "Impersonation ended; the admin's own session is restored", body = StatusAck),
    ),
    security(("session_cookie" = [])),
)]
pub async fn stop_impersonation(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // Capture the impersonated (target) user id before we restore the
    // admin's identity, so we can attribute the impersonation_end event
    // to them. Best-effort — if missing, we just skip the row.
    let impersonated_user_id: Option<Uuid> = session
        .get(SESSION_KEY_USER_ID)
        .await
        .ok()
        .flatten();
    let real_user_id: Option<Uuid> = session
        .get(SESSION_KEY_REAL_USER_ID)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let real_user_id =
        real_user_id.ok_or_else(|| ApiError::Validation("not in an impersonated session".into()))?;

    let real_pw: Option<i64> = session
        .get(SESSION_KEY_REAL_PW_CHANGED_AT)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let real_pw = real_pw.ok_or_else(|| ApiError::Internal("missing real pw watermark".into()))?;

    // Restore the admin's identity.
    session
        .insert(SESSION_KEY_USER_ID, real_user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .insert(SESSION_KEY_PW_CHANGED_AT, real_pw)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Clear impersonation keys.
    session
        .remove::<Uuid>(SESSION_KEY_REAL_USER_ID)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    session
        .remove::<i64>(SESSION_KEY_REAL_PW_CHANGED_AT)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(real_user_id),
            action: "admin.impersonate_stop",
            target_type: None,
            target_id: None,
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    // Phase 2 / Stage B — record under the impersonated user (mirrors
    // the impersonation_start attribution). The metadata carries the
    // admin who's stepping out.
    if let Some(target) = impersonated_user_id
        && let Err(e) = session_events::record(
            &state.pool,
            target,
            session_events::SessionEvent::ImpersonationEnd,
            crate::routes::auth::client_ip(&headers),
            crate::routes::auth::client_user_agent(&headers).as_deref(),
            json!({ "by_user_id": real_user_id }),
        )
        .await
        {
            warn!(?e, target = %target, "session_events impersonation_end record failed");
        }
    info!(admin = %real_user_id, "admin stopped impersonation");

    Ok(Json(json!({ "status": "ok" })))
}
