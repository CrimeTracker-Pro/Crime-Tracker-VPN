use std::{collections::HashMap, net::IpAddr, sync::Arc};

use askama::Template;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use garde::Validate;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use tracing::info;
use utoipa::ToSchema;
use uuid::Uuid;
use zerovpn_core::models::{Device, DeviceOs, DeviceStatus, DeviceType};
use zerovpn_db::repos::{audit, devices, servers, users};
use zerovpn_wg::{config, ip_alloc::IpAllocator, keys, qr};
use zerovpn_wire::{ChangeAction, Event, ResourceKind};

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::CurrentUser,
    routes::dto::StatusAck,
    state::AppState,
};

// Per-user active-device cap now lives in the DB (`users.device_limit`,
// admin-settable, default 5) — see `users::device_limit`.
pub const PERSISTENT_KEEPALIVE: u16 = 30;

/// A peer is considered "online" while its last handshake is within this
/// window — the same 180s bound the worker and the frontend `connState` use.
/// The app calls `POST /devices/connect` to (re)fetch its config on every
/// launch / Connect tap; we only record a `device.reconnected` event when the
/// device was actually offline (handshake stale or never seen) so an
/// already-connected peer re-fetching doesn't spam the activity log.
const ONLINE_HANDSHAKE_WINDOW_SECS: i64 = 180;

/// Broadcast a device mutation so the owner's other logged-in sessions (and
/// any admin watching) invalidate their device queries and reflect the change
/// in real time — e.g. a peer added on a phone appears on the laptop instantly.
/// Fire-and-forget; `device_id` is `None` for bulk operations like reorder.
pub(crate) fn notify_device(
    state: &AppState,
    user_id: Uuid,
    device_id: Option<Uuid>,
    action: ChangeAction,
) {
    state.broadcast(Event::DataChanged {
        user_id: Some(user_id),
        resource: ResourceKind::Device,
        id: device_id,
        action,
    });
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct CreateBody {
    #[garde(length(min = 1, max = 64))]
    pub name: String,
    #[garde(skip)]
    pub os: Option<DeviceOs>,
    /// Device form factor (phone, tablet, laptop, …). Independent of `os`.
    #[garde(skip)]
    pub device_type: Option<DeviceType>,
    /// Optional caller-supplied address. When set, the API tries to
    /// reserve exactly this IP in the server's CIDR rather than picking
    /// the next free one. Returns 409 if the address is taken or
    /// reserved (network / broadcast / gateway), 400 if it parses but
    /// isn't inside the server's CIDR.
    #[garde(skip)]
    pub allocated_ip: Option<String>,
    /// Optional per-device monthly byte cap. When the body omits this
    /// field, the device inherits the user's own monthly cap so it
    /// stays bounded by the account quota by default. An explicit value
    /// is clamped to the user's cap (a device can't exceed the account).
    /// Null/0 with a finite user cap still defaults to the user cap;
    /// null/0 with an unlimited account means no device cap.
    #[garde(skip)]
    pub monthly_byte_cap: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedDevice {
    pub device: PublicDevice,
    /// Rendered WireGuard .conf (full text). Shown to the user once at
    /// create / rotate / redownload time.
    pub config: String,
    /// SVG of the .conf as a QR code (for mobile clients).
    pub qr_svg: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PublicDevice {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub os: DeviceOs,
    pub device_type: DeviceType,
    pub public_key: String,
    #[schema(value_type = String, example = "10.10.0.5")]
    pub allocated_ip: IpAddr,
    pub status: DeviceStatus,
    pub server_id: Uuid,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_handshake_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Public `host:port` the peer last connected from, as observed by the
    /// WG poller. `None` until the device's first handshake; persists as
    /// the *last* endpoint after the peer goes offline. Not part of the
    /// core `Device` model — the list/get handlers merge it in (see
    /// `devices::peer_endpoints_for_user`). `None` on create/rotate/admin
    /// responses, which don't fetch it.
    pub last_peer_endpoint: Option<String>,
    /// Wall-clock time `last_peer_endpoint` was first observed.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_peer_endpoint_at: Option<OffsetDateTime>,
    /// All-time bytes received / sent, summed from the persisted hourly
    /// bandwidth rollups — so the card's RX/TX TOTAL survives a page reload
    /// (unlike the session-local live counters). `0` until the device has
    /// transmitted. Merged in by the list/get handlers; `0` on
    /// create/rotate responses, which don't fetch it.
    pub total_rx_bytes: i64,
    pub total_tx_bytes: i64,
    /// Per-device monthly byte cap (null/0 = no device cap; the account cap
    /// still applies). Admin-set; merged in by list/get, `None` elsewhere.
    pub monthly_byte_cap: Option<i64>,
    /// This device's own bandwidth used in the current monthly cycle. Merged
    /// in by list/get; `0` on create/rotate responses.
    pub current_month_bytes: i64,
    /// True when the quota sweep auto-paused this device (its own cap or the
    /// account cap), vs. a manual user pause.
    pub auto_paused: bool,
}

impl From<Device> for PublicDevice {
    fn from(d: Device) -> Self {
        Self {
            id: d.id,
            user_id: d.user_id,
            name: d.name,
            os: d.os,
            device_type: d.device_type,
            public_key: d.public_key,
            allocated_ip: d.allocated_ip.ip(),
            status: d.status,
            server_id: d.server_id,
            last_handshake_at: d.last_handshake_at,
            created_at: d.created_at,
            // Endpoint columns aren't on the core Device row; handlers that
            // want them merge them in after the conversion.
            last_peer_endpoint: None,
            last_peer_endpoint_at: None,
            // Totals come from bandwidth_aggregates; merged in by list/get.
            total_rx_bytes: 0,
            total_tx_bytes: 0,
            // Quota columns aren't on the core Device row; merged in by list/get.
            monthly_byte_cap: None,
            current_month_bytes: 0,
            auto_paused: false,
        }
    }
}

/// Split-tunnel AllowedIPs: every device routes only the VPN's own subnet
/// through the tunnel (peers + the gateway); all other traffic uses the
/// client's normal interface. This must be derived from the server record so
/// a CIDR correction is reflected in every newly rendered client profile.

#[utoipa::path(
    get,
    path = "/devices",
    tag = "Devices",
    responses(
        (status = 200, description = "User's devices in display order", body = Vec<PublicDevice>),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let rows = devices::list_for_user(&state.pool, user.id).await?;
    let mut out: Vec<PublicDevice> = rows.into_iter().map(Into::into).collect();
    // Merge in the WG peer endpoint (latest host:port the poller saw).
    // Kept off the core Device model, so fetched once for the user and
    // zipped onto each card here.
    let endpoints: HashMap<Uuid, (Option<String>, Option<OffsetDateTime>)> =
        devices::peer_endpoints_for_user(&state.pool, user.id)
            .await?
            .into_iter()
            .map(|(id, ep, at)| (id, (ep, at)))
            .collect();
    // All-time per-device byte totals (persisted; survives reload). One
    // grouped query for the whole list, zipped on per device.
    let totals: HashMap<Uuid, (i64, i64)> =
        zerovpn_db::repos::bandwidth::device_totals(&state.pool, user.id)
            .await?
            .into_iter()
            .map(|(id, rx, tx)| (id, (rx, tx)))
            .collect();
    // Per-device monthly quota (cap + usage + auto-pause flag) for the card.
    let quotas: HashMap<Uuid, (Option<i64>, i64, bool)> =
        devices::quota_for_user(&state.pool, user.id)
            .await?
            .into_iter()
            .map(|(id, cap, used, paused)| (id, (cap, used, paused)))
            .collect();
    for d in &mut out {
        if let Some((ep, at)) = endpoints.get(&d.id) {
            d.last_peer_endpoint = ep.clone();
            d.last_peer_endpoint_at = *at;
        }
        if let Some((rx, tx)) = totals.get(&d.id) {
            d.total_rx_bytes = *rx;
            d.total_tx_bytes = *tx;
        }
        if let Some((cap, used, paused)) = quotas.get(&d.id) {
            d.monthly_byte_cap = *cap;
            d.current_month_bytes = *used;
            d.auto_paused = *paused;
        }
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/devices/{id}",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Device row", body = PublicDevice),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn get(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let d = devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut pd = PublicDevice::from(d);
    // Merge in the latest WG peer endpoint (see `list`). ≤5 rows per user,
    // so a single fetch + find is cheaper than its own indexed query.
    if let Some((_, ep, at)) = devices::peer_endpoints_for_user(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|(eid, _, _)| *eid == id)
    {
        pd.last_peer_endpoint = ep;
        pd.last_peer_endpoint_at = at;
    }
    // All-time byte totals (persisted) — same source the list uses.
    if let Some((_, rx, tx)) = zerovpn_db::repos::bandwidth::device_totals(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|(did, _, _)| *did == id)
    {
        pd.total_rx_bytes = rx;
        pd.total_tx_bytes = tx;
    }
    // Per-device monthly quota — same source the list uses.
    if let Some((_, cap, used, paused)) = devices::quota_for_user(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|(did, _, _, _)| *did == id)
    {
        pd.monthly_byte_cap = cap;
        pd.current_month_bytes = used;
        pd.auto_paused = paused;
    }
    Ok(Json(pd))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceEvent {
    pub id: i64,
    pub action: String,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

#[derive(Debug, Deserialize, Default, utoipa::IntoParams)]
pub struct EventsQuery {
    /// Page size. Clamped to [1, 500] server-side. Defaults to 100 when
    /// omitted — enough to render a full day of typical activity.
    pub limit: Option<i64>,
    /// Keyset cursor for infinite scroll: return only events older than this
    /// id (use the last row's id from the previous page). Omit for page 1.
    pub before_id: Option<i64>,
}

/// Returns the audit-log entries targeting this device, newest first.
/// Powers the device-detail "Activity" timeline: lifecycle events
/// (created / paused / unpaused / revoked), config changes, key
/// rotations, conf re-downloads, and the worker-emitted online/offline
/// transitions. Ownership is enforced — the caller must own the device.
#[utoipa::path(
    get,
    path = "/devices/{id}/events",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
        EventsQuery,
    ),
    responses(
        (status = 200, description = "Audit entries targeting this device, newest first", body = Vec<DeviceEvent>),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<impl IntoResponse> {
    // 404 (not 403) for devices the caller doesn't own — keeps the
    // existence of other users' device ids opaque, matching `get`.
    devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let limit = q.limit.unwrap_or(100);
    let rows =
        audit::list_for_target_before(&state.pool, "device", id, q.before_id, limit).await?;
    let out: Vec<DeviceEvent> = rows
        .into_iter()
        .map(|r| DeviceEvent {
            id: r.id,
            action: r.action,
            metadata: r.metadata,
            created_at: r.created_at,
        })
        .collect();
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/devices",
    tag = "Devices",
    request_body = CreateBody,
    responses(
        (status = 201, description = "Device created with fresh keypair + .conf + QR (shown ONCE)", body = CreatedDevice),
        (status = 400, description = "Validation error / invalid IP"),
        (status = 409, description = "Per-user device cap hit or chosen IP unavailable"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    body.validate().map_err(|e| ApiError::Validation(e.to_string()))?;

    // Enforce the per-user device cap (admin-settable; defaults to 5).
    let limit = users::device_limit(&state.pool, user.id).await? as usize;
    let existing = devices::list_for_user(&state.pool, user.id).await?;
    if existing.len() >= limit {
        return Err(ApiError::Conflict(format!(
            "device limit reached ({limit} active devices per user)"
        )));
    }

    // Pick the first active server. Multi-server choice arrives in v2.
    let server = servers::list_active(&state.pool)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::Internal("no active servers".into()))?;

    let alloc = state
        .allocators
        .get(server.id)
        .ok_or_else(|| ApiError::Internal("server allocator missing".into()))?;
    let ip = match body.allocated_ip.as_ref() {
        Some(raw) => reserve_specific(&alloc, raw)?,
        None => allocate_auto(&alloc)?,
    };

    let private_key = keys::generate_private_key();
    let public_key = keys::derive_public_key(&private_key)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // The private key is always stored (KEK-encrypted) on the device row so
    // the user can re-download the .conf later from any client.
    let private_key_encrypted = state
        .kek
        .encrypt(private_key.as_bytes())
        .map_err(|e| ApiError::Internal(format!("encrypt private key: {e}")))?;

    // Best-effort: persist the device row. If it fails we must release the IP.
    let host_prefix = if ip.is_ipv4() { 32 } else { 128 };
    let allocated_cidr = IpNetwork::new(ip, host_prefix).expect("valid host prefix");
    let device_id = match devices::create(
        &state.pool,
        devices::NewDevice {
            user_id: user.id,
            server_id: server.id,
            name: &body.name,
            os: body.os.unwrap_or(DeviceOs::Other),
            device_type: body.device_type.unwrap_or(DeviceType::Other),
            public_key: &public_key,
            preshared_key_encrypted: None, // will encrypt with KEK in a follow-up
            allocated_ip: allocated_cidr,
            private_key_encrypted: Some(&private_key_encrypted),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // Best-effort release.
            let _ = alloc.release(ip);
            return Err(e.into());
        }
    };

    // Seed the per-device monthly cap. If the body supplied one, it's
    // clamped to the user's account cap (a device can't exceed the
    // account quota). If omitted, the device inherits the user's cap by
    // default so a new device is automatically bounded by the account.
    // None ↔ 0 in admin semantics both mean "no per-device cap" — when
    // the user has no account cap and the body omits the value we leave
    // the device row's cap null.
    let user_cap = users::month_quota(&state.pool, user.id)
        .await?
        .and_then(|(c, _, _)| c)
        .filter(|c| *c > 0);
    let requested_cap = body.monthly_byte_cap.filter(|c| *c > 0);
    let effective_cap = match (requested_cap, user_cap) {
        (Some(req), Some(uc)) => Some(req.min(uc)),
        (Some(req), None) => Some(req),
        (None, Some(uc)) => Some(uc),
        (None, None) => None,
    };
    if let Some(cap) = effective_cap {
        devices::set_quota(&state.pool, device_id, Some(cap)).await?;
    }

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.created",
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({
                "name": body.name,
                "server": server.name,
                "monthly_byte_cap": effective_cap,
            }),
            ip: None,
        },
    )
    .await?;

    // Render the WG config for the user. The private key is held only here,
    // never persisted.
    let endpoint_str = format!("{}:{}", server.endpoint_host, server.endpoint_port);
    let address_str = format!("{}/32", ip);
    let allowed_ips = server.cidr.to_string();

    let keepalive = server.persistent_keepalive as u16;
    let cfg = config::PeerConfig {
        private_key: &private_key,
        address: &address_str,
        mtu: Some(server.mtu as u16),
        server_public_key: &server.public_key,
        preshared_key: None,
        allowed_ips: &allowed_ips,
        endpoint: &endpoint_str,
        keepalive,
    };
    let conf_text = cfg
        .render()
        .map_err(|e| ApiError::Internal(format!("render conf: {e}")))?;
    let qr_svg = qr::render_svg(&conf_text)
        .map_err(|e| ApiError::Internal(format!("render qr: {e}")))?;

    let stored = devices::find_for_user(&state.pool, user.id, device_id)
        .await?
        .ok_or_else(|| ApiError::Internal("just-created device missing".into()))?;

    // Hand the new peer to the running WG interface (Noop in dev).
    if let Err(e) = state
        .wg
        .add_peer(&public_key, ip, None, keepalive)
        .await
    {
        tracing::warn!(?e, "wg add_peer failed (continuing — DB row persisted)");
    }

    info!(user_id = %user.id, device_id = %device_id, ip = %ip, "device created");
    metrics::counter!("zerovpn_devices_created").increment(1);
    notify_device(&state, user.id, Some(device_id), ChangeAction::Created);

    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreatedDevice {
            device: stored.into(),
            config: conf_text,
            qr_svg,
        }),
    ))
}

/// Generates a fresh keypair for an existing (non-revoked) device, swaps
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReorderBody {
    pub ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReorderAck {
    #[schema(example = "ok")]
    pub status: &'static str,
    /// Rows actually updated. Ids the caller doesn't own are silently
    /// filtered out by the UPDATE, so `updated < ids.len()` simply means
    /// some ids were unknown.
    pub updated: u64,
}

/// Persist the user's preferred device order. Caller sends the full list
/// of device ids in the new order; we bulk-assign `display_order` so all
/// of the user's sessions see the same arrangement on next /devices
/// fetch. Ignores ids that don't belong to the caller (the UPDATE's
/// `user_id` clause filters them out) so a malicious body can't reorder
/// another user's devices.
#[utoipa::path(
    put,
    path = "/devices/order",
    tag = "Devices",
    request_body = ReorderBody,
    responses(
        (status = 200, description = "Display order persisted", body = ReorderAck),
        (status = 400, description = "Too many ids (>500)"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn reorder(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ReorderBody>,
) -> ApiResult<impl IntoResponse> {
    if body.ids.is_empty() {
        return Ok(Json(json!({ "status": "ok", "updated": 0 })));
    }
    if body.ids.len() > 500 {
        return Err(ApiError::Validation(
            "too many ids; reorder accepts at most 500".into(),
        ));
    }
    let n = devices::set_display_order(&state.pool, user.id, &body.ids).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.reordered",
            target_type: Some("device"),
            target_id: None,
            metadata: json!({ "count": body.ids.len() }),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, count = body.ids.len(), "devices reordered");
    notify_device(&state, user.id, None, ChangeAction::Reordered);
    Ok(Json(json!({ "status": "ok", "updated": n })))
}

/// the public key on the row, updates the running WG interface, and
/// returns the rendered config + QR so the user can scan the new
/// credentials on their device. Mirrors the `create` response shape so
/// the same dialog UI can render it.
#[utoipa::path(
    post,
    path = "/devices/{id}/rotate-keys",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "New keypair, conf, and QR (shown ONCE)", body = CreatedDevice),
        (status = 404, description = "Not found / not owned"),
        (status = 409, description = "Device is revoked"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn rotate_keys(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let device = devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if device.status == DeviceStatus::Revoked {
        return Err(ApiError::Conflict("revoked device cannot rotate keys".into()));
    }

    let server = zerovpn_db::repos::servers::find_by_id(&state.pool, device.server_id)
        .await?
        .ok_or_else(|| ApiError::Internal("device's server missing".into()))?;

    let old_public_key = device.public_key.clone();
    let private_key = keys::generate_private_key();
    let public_key = keys::derive_public_key(&private_key)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let n = devices::update_public_key(&state.pool, user.id, id, &public_key).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }

    // The private key is always stored (KEK-encrypted) so the user can
    // re-download the .conf later.
    let encrypted = state
        .kek
        .encrypt(private_key.as_bytes())
        .map_err(|e| ApiError::Internal(format!("encrypt rotated key: {e}")))?;
    devices::set_private_key_encrypted(&state.pool, user.id, id, Some(&encrypted)).await?;

    // Swap the peer on the running WG interface. Best-effort — DB is the
    // source of truth; if WG control is a no-op (dev) the new config will
    // still match what's persisted.
    if let Err(e) = state.wg.remove_peer(&old_public_key).await {
        tracing::warn!(?e, "wg remove_peer (rotate) failed");
    }
    let keepalive = server.persistent_keepalive as u16;
    if let Err(e) = state
        .wg
        .add_peer(
            &public_key,
            device.allocated_ip.ip(),
            None,
            keepalive,
        )
        .await
    {
        tracing::warn!(?e, "wg add_peer (rotate) failed");
    }

    // Render the new wg-conf using whatever the device's stored overrides
    // already say. No mock fields — the user gets exactly what their
    // current settings dictate, just with a fresh private key.
    let endpoint_str = format!("{}:{}", server.endpoint_host, server.endpoint_port);
    let address_str = format!("{}/32", device.allocated_ip.ip());
    let allowed_ips = server.cidr.to_string();
    let cfg = config::PeerConfig {
        private_key: &private_key,
        address: &address_str,
        mtu: Some(server.mtu as u16),
        server_public_key: &server.public_key,
        preshared_key: None,
        allowed_ips: &allowed_ips,
        endpoint: &endpoint_str,
        keepalive,
    };
    let conf_text = cfg
        .render()
        .map_err(|e| ApiError::Internal(format!("render conf: {e}")))?;
    let qr_svg = qr::render_svg(&conf_text)
        .map_err(|e| ApiError::Internal(format!("render qr: {e}")))?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.keys_rotated",
            target_type: Some("device"),
            target_id: Some(id),
            metadata: json!({ "name": device.name }),
            ip: None,
        },
    )
    .await?;

    let stored = devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or_else(|| ApiError::Internal("rotated device missing".into()))?;

    info!(user_id = %user.id, device_id = %id, "device keys rotated");
    notify_device(&state, user.id, Some(id), ChangeAction::KeysRotated);

    Ok(Json(CreatedDevice {
        device: stored.into(),
        config: conf_text,
        qr_svg,
    }))
}

#[utoipa::path(
    delete,
    path = "/devices/{id}",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Device revoked + IP released back to the allocator", body = StatusAck),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let device = devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    teardown_device(&state, &device).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.revoked",
            target_type: Some("device"),
            target_id: Some(id),
            metadata: json!({}),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, device_id = %id, "device revoked");
    notify_device(&state, user.id, Some(id), ChangeAction::Deleted);
    Ok(Json(json!({ "status": "ok" })))
}

/// Shared revoke core, used by the owner's DELETE and the admin revoke:
/// flips the row to `revoked`, releases the allocated IP, removes the WG
/// peer and releases its allocated IP.
/// Callers own audit + notify — actor and action name differ per path.
pub(crate) async fn teardown_device(state: &AppState, device: &Device) -> ApiResult<()> {
    let n =
        devices::set_status(&state.pool, device.user_id, device.id, DeviceStatus::Revoked).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    if let Some(alloc) = state.allocators.get(device.server_id) {
        let _ = alloc.release(device.allocated_ip.ip());
    }
    if let Err(e) = state.wg.remove_peer(&device.public_key).await {
        tracing::warn!(?e, "wg remove_peer failed (non-fatal)");
    }
    metrics::counter!("zerovpn_devices_revoked").increment(1);
    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchBody {
    pub name: Option<String>,
    pub os: Option<DeviceOs>,
    pub device_type: Option<DeviceType>,
}

/// Re-render the device's .conf from the server-stored private key.
/// Returns 404 if the device doesn't exist, 409 if the device was
/// created without `store_private_key` (no key to recover). Owner-only;
/// audit-logged so a stolen session leaves a paper trail.
#[utoipa::path(
    get,
    path = "/devices/{id}/conf",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Re-rendered .conf + QR using current overrides", body = CreatedDevice),
        (status = 404, description = "Not found / not owned"),
        (status = 409, description = "Device is revoked, or was not created with stored private key"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn redownload_conf(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let device = devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if device.status == DeviceStatus::Revoked {
        return Err(ApiError::Conflict("device is revoked".into()));
    }
    let encrypted = device
        .private_key_encrypted
        .as_ref()
        .ok_or_else(|| {
            ApiError::Conflict(
                "device was not created with server-side key storage; rotate keys to re-issue"
                    .into(),
            )
        })?;
    let private_bytes = state
        .kek
        .decrypt(encrypted)
        .map_err(|e| ApiError::Internal(format!("decrypt private key: {e}")))?;
    let private_key = String::from_utf8(private_bytes)
        .map_err(|_| ApiError::Internal("stored private key is not UTF-8".into()))?;

    let server = zerovpn_db::repos::servers::find_by_id(&state.pool, device.server_id)
        .await?
        .ok_or_else(|| ApiError::Internal("device's server missing".into()))?;

    // Same conf-render path as `create` + `rotate_keys`. Pulls overrides
    // from the device row so the re-download reflects the user's current
    // configuration, not the one captured at create time.
    let endpoint_str = format!("{}:{}", server.endpoint_host, server.endpoint_port);
    let address_str = format!("{}/32", device.allocated_ip.ip());
    let allowed_ips = server.cidr.to_string();
    let cfg = config::PeerConfig {
        private_key: &private_key,
        address: &address_str,
        mtu: Some(server.mtu as u16),
        server_public_key: &server.public_key,
        preshared_key: None,
        allowed_ips: &allowed_ips,
        endpoint: &endpoint_str,
        keepalive: server.persistent_keepalive as u16,
    };
    let conf_text = cfg
        .render()
        .map_err(|e| ApiError::Internal(format!("render conf: {e}")))?;
    let qr_svg = qr::render_svg(&conf_text)
        .map_err(|e| ApiError::Internal(format!("render qr: {e}")))?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.conf_redownloaded",
            target_type: Some("device"),
            target_id: Some(id),
            metadata: json!({ "name": device.name }),
            ip: None,
        },
    )
    .await?;

    Ok(Json(CreatedDevice {
        device: device.into(),
        config: conf_text,
        qr_svg,
    }))
}

// ── App one-tap connect ───────────────────────────────────────────────────

/// Connect/provision request from the mobile/desktop app's "Connect" button.
///
/// - `device_id` **absent** → provision: the server generates the keypair,
///   stores it KEK-encrypted (so later reconnects can re-serve the profile),
///   registers the peer, and returns the connection profile.
/// - `device_id` **present** → reconnect: re-serve that device's profile and
///   re-assert the peer on the live interface, without creating a new row.
///   If the id is unknown or points at a revoked device, the server falls
///   back to provisioning a fresh one (and returns the new `device_id`), so
///   a stale id never blocks Connect — check `reused` to tell which happened.
///
/// The app authenticates with the same session it gets from the normal
/// login flow (no separate token system). Keep `device_id` from the latest
/// response in local secure storage and send it back on every later Connect
/// so the user accumulates exactly one device, not one per tap.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectBody {
    /// Existing device to reconnect. Omit on first connect.
    pub device_id: Option<Uuid>,
    /// Display name for a newly provisioned device (e.g. the OS hostname).
    /// Ignored on reconnect; defaults to a label derived from os/type.
    pub name: Option<String>,
    pub os: Option<DeviceOs>,
    pub device_type: Option<DeviceType>,
}

/// Structured WireGuard parameters the native client brings the tunnel up
/// with directly — no `.conf` parsing required. (`config`/`qr_svg` on the
/// envelope carry the same data for manual import / QR.)
#[derive(Debug, Serialize, ToSchema)]
pub struct WgProfile {
    /// Client private key (server-generated for this flow).
    pub private_key: String,
    /// Interface address in CIDR form.
    #[schema(example = "10.10.0.5/32")]
    pub address: String,
    pub server_public_key: String,
    /// Server `host:port` to dial.
    #[schema(example = "vpn.example.com:51820")]
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    pub mtu: u16,
    pub persistent_keepalive: u16,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectResponse {
    pub device: PublicDevice,
    /// Structured params for the native client.
    pub profile: WgProfile,
    /// Full rendered `.conf` (manual import / desktop WireGuard app).
    pub config: String,
    /// QR of the `.conf` for camera-based import.
    pub qr_svg: String,
    /// `true` when an existing device was reused (reconnect), `false` when a
    /// new device was provisioned.
    pub reused: bool,
}

/// Default display name when the app doesn't send one — readable and stable,
/// e.g. "macOS laptop", "Android phone", "iOS".
fn default_device_name(os: DeviceOs, dt: DeviceType) -> String {
    let os_label = match os {
        DeviceOs::Ios => "iOS",
        DeviceOs::Android => "Android",
        DeviceOs::Macos => "macOS",
        DeviceOs::Windows => "Windows",
        DeviceOs::Linux => "Linux",
        DeviceOs::Other => "Device",
    };
    let dt_label = match dt {
        DeviceType::Phone => "phone",
        DeviceType::Tablet => "tablet",
        DeviceType::Laptop => "laptop",
        DeviceType::Desktop => "desktop",
        DeviceType::Tv => "TV",
        DeviceType::Router => "router",
        DeviceType::Watch => "watch",
        DeviceType::Iot => "IoT",
        DeviceType::Server => "server",
        DeviceType::Other => "",
    };
    if dt_label.is_empty() {
        os_label.to_string()
    } else {
        format!("{os_label} {dt_label}")
    }
}

/// Already-resolved connection parameters for [`render_profile`].
struct ProfileParams<'a> {
    private_key: &'a str,
    address: &'a str,
    server_public_key: &'a str,
    endpoint: &'a str,
    allowed_ips: &'a [String],
    mtu: u16,
    keepalive: u16,
}

/// Build the structured profile + rendered `.conf` + QR from already-resolved
/// connection parameters. Shared by the connect handler's provision and
/// reconnect branches so the WG-render path lives in exactly one place.
fn render_profile(
    ProfileParams {
        private_key,
        address,
        server_public_key,
        endpoint,
        allowed_ips,
        mtu,
        keepalive,
    }: ProfileParams<'_>,
) -> ApiResult<(WgProfile, String, String)> {
    let allowed_joined = allowed_ips.join(", ");
    let cfg = config::PeerConfig {
        private_key,
        address,
        mtu: Some(mtu),
        server_public_key,
        preshared_key: None,
        allowed_ips: &allowed_joined,
        endpoint,
        keepalive,
    };
    let config = cfg
        .render()
        .map_err(|e| ApiError::Internal(format!("render conf: {e}")))?;
    let qr_svg =
        qr::render_svg(&config).map_err(|e| ApiError::Internal(format!("render qr: {e}")))?;
    let profile = WgProfile {
        private_key: private_key.to_string(),
        address: address.to_string(),
        server_public_key: server_public_key.to_string(),
        endpoint: endpoint.to_string(),
        allowed_ips: allowed_ips.to_vec(),
        mtu,
        persistent_keepalive: keepalive,
    };
    Ok((profile, config, qr_svg))
}

/// Split-tunnel AllowedIPs (the VPN subnet) for an app-provisioned device.
fn default_allowed_ips(server_cidr: impl ToString) -> Vec<String> {
    vec![server_cidr.to_string()]
}

#[utoipa::path(
    post,
    path = "/devices/connect",
    tag = "Devices",
    request_body = ConnectBody,
    responses(
        (status = 200, description = "Reconnected — existing device profile re-served", body = ConnectResponse),
        (status = 201, description = "Provisioned — new device + profile (also when device_id is stale/revoked)", body = ConnectResponse),
        (status = 409, description = "Per-user device cap reached"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn connect(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ConnectBody>,
) -> ApiResult<impl IntoResponse> {
    // ── Reconnect: re-serve a live device's profile ────────────────────────
    // Only when device_id resolves to a non-revoked device that still has a
    // stored key. A missing / revoked / keyless id falls through to
    // provisioning a fresh device below — so a stale id on the app (e.g. the
    // device was deleted, which soft-revokes it) never wedges the Connect
    // button with a hard error; it just transparently re-provisions.
    let live_device = match body.device_id {
        Some(device_id) => devices::find_for_user(&state.pool, user.id, device_id)
            .await?
            .filter(|d| d.status != DeviceStatus::Revoked && d.private_key_encrypted.is_some()),
        None => None,
    };
    if let Some(device) = live_device {
        let device_id = device.id;
        let encrypted = device
            .private_key_encrypted
            .as_ref()
            .expect("filtered to a device with a stored key above");
        let private_bytes = state
            .kek
            .decrypt(encrypted)
            .map_err(|e| ApiError::Internal(format!("decrypt private key: {e}")))?;
        let private_key = String::from_utf8(private_bytes)
            .map_err(|_| ApiError::Internal("stored private key is not UTF-8".into()))?;

        let server = zerovpn_db::repos::servers::find_by_id(&state.pool, device.server_id)
            .await?
            .ok_or_else(|| ApiError::Internal("device's server missing".into()))?;

        let ip = device.allocated_ip.ip();
        let address = format!("{}/32", ip);
        let allowed_ips = default_allowed_ips(server.cidr);
        let endpoint = format!("{}:{}", server.endpoint_host, server.endpoint_port);
        let keepalive = server.persistent_keepalive as u16;
        let (profile, config, qr_svg) = render_profile(ProfileParams {
            private_key: &private_key,
            address: &address,
            server_public_key: &server.public_key,
            endpoint: &endpoint,
            allowed_ips: &allowed_ips,
            mtu: server.mtu as u16,
            keepalive,
        })?;

        // Re-assert the peer on the live interface (idempotent) so the tunnel
        // works even if the interface/worker restarted since provisioning.
        if let Err(e) = state
            .wg
            .add_peer(&device.public_key, ip, None, keepalive)
            .await
        {
            tracing::warn!(?e, %device_id, "wg add_peer on reconnect failed (continuing)");
        }

        // Only treat this as a real reconnect when the device was actually
        // offline (no handshake on record, or the last one is older than the
        // online window). An already-online peer re-fetching its config is a
        // no-op for the activity feed — recording it every time produced a
        // stream of meaningless "device.reconnected" entries.
        let now = OffsetDateTime::now_utc();
        let was_offline = match device.last_handshake_at {
            Some(ts) => (now - ts).whole_seconds() >= ONLINE_HANDSHAKE_WINDOW_SECS,
            None => true,
        };
        if was_offline {
            audit::record(
                &state.pool,
                audit::AuditEntry {
                    actor_user_id: Some(user.id),
                    action: "device.reconnected",
                    target_type: Some("device"),
                    target_id: Some(device_id),
                    metadata: json!({ "name": device.name }),
                    ip: None,
                },
            )
            .await?;
            info!(user_id = %user.id, device_id = %device_id, "device reconnected");
            notify_device(&state, user.id, Some(device_id), ChangeAction::Connected);
        } else {
            tracing::debug!(
                user_id = %user.id,
                device_id = %device_id,
                "connect: config re-fetch for already-online device (no reconnect logged)"
            );
        }
        return Ok((
            axum::http::StatusCode::OK,
            Json(ConnectResponse {
                device: device.into(),
                profile,
                config,
                qr_svg,
                reused: true,
            }),
        ));
    }

    // ── Provision: register a new device, server-generated keys ────────────
    let limit = users::device_limit(&state.pool, user.id).await? as usize;
    let existing = devices::list_for_user(&state.pool, user.id).await?;
    if existing.len() >= limit {
        return Err(ApiError::Conflict(format!(
            "device limit reached ({limit} active devices per user)"
        )));
    }

    let os = body.os.unwrap_or(DeviceOs::Other);
    let device_type = body.device_type.unwrap_or(DeviceType::Other);
    let name = match body.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => {
            if n.chars().count() > 64 {
                return Err(ApiError::Validation("name must be 1–64 characters".into()));
            }
            n.to_string()
        }
        _ => default_device_name(os, device_type),
    };

    let server = servers::list_active(&state.pool)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::Internal("no active servers".into()))?;
    let alloc = state
        .allocators
        .get(server.id)
        .ok_or_else(|| ApiError::Internal("server allocator missing".into()))?;
    let ip = allocate_auto(&alloc)?;

    let private_key = keys::generate_private_key();
    let public_key =
        keys::derive_public_key(&private_key).map_err(|e| ApiError::Internal(e.to_string()))?;
    // Always store the key (KEK-encrypted) — reconnect re-serves it, and the
    // server generated it so there's no client copy to fall back on.
    let private_key_encrypted = state
        .kek
        .encrypt(private_key.as_bytes())
        .map_err(|e| ApiError::Internal(format!("encrypt private key: {e}")))?;

    let host_prefix = if ip.is_ipv4() { 32 } else { 128 };
    let allocated_cidr = IpNetwork::new(ip, host_prefix).expect("valid host prefix");
    let device_id = match devices::create(
        &state.pool,
        devices::NewDevice {
            user_id: user.id,
            server_id: server.id,
            name: &name,
            os,
            device_type,
            public_key: &public_key,
            preshared_key_encrypted: None,
            allocated_ip: allocated_cidr,
            private_key_encrypted: Some(&private_key_encrypted),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            let _ = alloc.release(ip);
            return Err(e.into());
        }
    };

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.connected",
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({ "name": name, "server": server.name, "source": "app" }),
            ip: None,
        },
    )
    .await?;

    let address = format!("{}/32", ip);
    let allowed_ips = default_allowed_ips(server.cidr);
    let endpoint = format!("{}:{}", server.endpoint_host, server.endpoint_port);
    let keepalive = server.persistent_keepalive as u16;
    let (profile, config, qr_svg) = render_profile(ProfileParams {
        private_key: &private_key,
        address: &address,
        server_public_key: &server.public_key,
        endpoint: &endpoint,
        allowed_ips: &allowed_ips,
        mtu: server.mtu as u16,
        keepalive,
    })?;

    let stored = devices::find_for_user(&state.pool, user.id, device_id)
        .await?
        .ok_or_else(|| ApiError::Internal("just-created device missing".into()))?;

    if let Err(e) = state
        .wg
        .add_peer(&public_key, ip, None, keepalive)
        .await
    {
        tracing::warn!(?e, "wg add_peer failed (continuing — DB row persisted)");
    }

    info!(user_id = %user.id, device_id = %device_id, ip = %ip, "device connected (app provision)");
    notify_device(&state, user.id, Some(device_id), ChangeAction::Created);
    Ok((
        axum::http::StatusCode::CREATED,
        Json(ConnectResponse {
            device: stored.into(),
            profile,
            config,
            qr_svg,
            reused: false,
        }),
    ))
}

#[utoipa::path(
    patch,
    path = "/devices/{id}",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    request_body = PatchBody,
    responses(
        (status = 200, description = "Device updated", body = StatusAck),
        (status = 400, description = "Validation error (bad CIDR / IP)"),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn patch(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> ApiResult<impl IntoResponse> {
    devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(ref name) = body.name
        && (name.trim().is_empty() || name.len() > 64) {
            return Err(ApiError::Validation("name must be 1–64 chars".into()));
        }
    sqlx::query(
        r#"UPDATE devices
              SET name = COALESCE($3, name),
                  os = COALESCE($4, os),
                  device_type = COALESCE($5, device_type)
            WHERE user_id = $1 AND id = $2"#,
    )
    .bind(user.id)
    .bind(id)
    .bind(body.name.as_deref())
    .bind(body.os)
    .bind(body.device_type)
    .execute(&state.pool)
    .await?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.updated",
            target_type: Some("device"),
            target_id: Some(id),
            metadata: json!({
                "name_changed": body.name.is_some(),
                "os_changed": body.os.is_some(),
            }),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, device_id = %id, "device patched");
    notify_device(&state, user.id, Some(id), ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeviceQuotaBody {
    /// Per-device cap in bytes for the current month. Null/0 = no
    /// per-device cap (the account cap still applies). Clamped to the
    /// caller's own account cap server-side — a user can't grant a
    /// device more headroom than their own quota.
    pub monthly_byte_cap: Option<i64>,
}

/// User-facing per-device quota setter. Lets the device owner cap their
/// own device below the account quota (e.g. throttle a phone separately
/// from a laptop). Distinct from the admin endpoint
/// `PUT /admin/devices/{id}/quota` because the user route enforces
/// ownership *and* clamps to the user's own cap, while the admin route
/// can set any cap (including above the owner's account cap, in case
/// the admin is also raising the account cap in the same flow).
#[utoipa::path(
    put,
    path = "/devices/{id}/quota",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    request_body = DeviceQuotaBody,
    responses(
        (status = 200, description = "Per-device cap updated (clamped to account cap)", body = StatusAck),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_my_quota(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DeviceQuotaBody>,
) -> ApiResult<impl IntoResponse> {
    devices::find_for_user(&state.pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let user_cap = users::month_quota(&state.pool, user.id)
        .await?
        .and_then(|(c, _, _)| c)
        .filter(|c| *c > 0);
    let requested = body.monthly_byte_cap.filter(|c| *c > 0);
    // Clamp to the user's own cap so a device can't exceed the account.
    let cap = match (requested, user_cap) {
        (Some(req), Some(uc)) => Some(req.min(uc)),
        (Some(req), None) => Some(req),
        (None, _) => None,
    };
    devices::set_quota(&state.pool, id, cap).await?;
    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user.id),
            action: "device.quota_set",
            target_type: Some("device"),
            target_id: Some(id),
            metadata: json!({ "monthly_byte_cap": cap }),
            ip: None,
        },
    )
    .await?;
    info!(user_id = %user.id, device_id = %id, ?cap, "user set device quota");
    notify_device(&state, user.id, Some(id), ChangeAction::Updated);
    Ok(Json(json!({ "status": "ok" })))
}

#[utoipa::path(
    post,
    path = "/devices/{id}/pause",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Device paused; peer removed from running WG", body = StatusAck),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn pause(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    set_pause_state(&state, user.id, id, DeviceStatus::Paused).await?;
    Ok(Json(json!({ "status": "paused" })))
}

#[utoipa::path(
    post,
    path = "/devices/{id}/unpause",
    tag = "Devices",
    params(
        ("id" = Uuid, Path, description = "Device UUID"),
    ),
    responses(
        (status = 200, description = "Device resumed; peer re-added to running WG", body = StatusAck),
        (status = 404, description = "Not found / not owned"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn unpause(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    set_pause_state(&state, user.id, id, DeviceStatus::Active).await?;
    Ok(Json(json!({ "status": "active" })))
}

async fn set_pause_state(
    state: &AppState,
    user_id: Uuid,
    device_id: Uuid,
    target: DeviceStatus,
) -> ApiResult<()> {
    let device = devices::find_for_user(&state.pool, user_id, device_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    apply_pause_state(state, &device, target).await?;

    audit::record(
        &state.pool,
        audit::AuditEntry {
            actor_user_id: Some(user_id),
            action: match target {
                DeviceStatus::Paused => "device.paused",
                DeviceStatus::Active => "device.unpaused",
                _ => "device.status_changed",
            },
            target_type: Some("device"),
            target_id: Some(device_id),
            metadata: json!({ "to": target }),
            ip: None,
        },
    )
    .await?;

    notify_device(
        state,
        user_id,
        Some(device_id),
        match target {
            DeviceStatus::Paused => ChangeAction::Paused,
            _ => ChangeAction::Unpaused,
        },
    );

    Ok(())
}

/// Shared pause/resume core, used by the owner's handlers and the admin
/// ones: flips the row and reflects the state onto the running WG
/// interface + resolver. Callers own audit + notify.
pub(crate) async fn apply_pause_state(
    state: &AppState,
    device: &Device,
    target: DeviceStatus,
) -> ApiResult<()> {
    if device.status == DeviceStatus::Revoked {
        return Err(ApiError::Conflict("revoked device cannot change status".into()));
    }
    let n = devices::set_status(&state.pool, device.user_id, device.id, target).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }

    // Reflect status to the running WG interface.
    match target {
        DeviceStatus::Paused => {
            if let Err(e) = state.wg.remove_peer(&device.public_key).await {
                tracing::warn!(?e, "wg remove_peer (pause) failed");
            }
        }
        DeviceStatus::Active => {
            // Look up the server to pull its per-server PersistentKeepalive —
            // falls back to the historical default if the lookup fails so a
            // transient DB blip doesn't block resume.
            let keepalive = match zerovpn_db::repos::servers::find_by_id(
                &state.pool,
                device.server_id,
            )
            .await
            {
                Ok(Some(s)) => s.persistent_keepalive as u16,
                Ok(None) | Err(_) => PERSISTENT_KEEPALIVE,
            };
            if let Err(e) = state
                .wg
                .add_peer(
                    &device.public_key,
                    device.allocated_ip.ip(),
                    None,
                    keepalive,
                )
                .await
            {
                tracing::warn!(?e, "wg add_peer (unpause) failed");
            }
        }
        _ => {}
    }

    Ok(())
}

fn allocate_auto(alloc: &Arc<IpAllocator>) -> ApiResult<IpAddr> {
    alloc
        .allocate()
        .map_err(|_| ApiError::Conflict("server IP pool exhausted".into()))
}

/// Caller-supplied IP path. Parses the string and asks the allocator
/// to atomically claim it inside the server's CIDR. Family must match
/// the server's CIDR; mismatches surface as a validation error so the
/// frontend can render a clear message. Maps the other interesting
/// outcomes to specific API errors so toasts can be targeted
/// ("already taken" vs "outside subnet" vs "reserved address").
fn reserve_specific(
    alloc: &Arc<IpAllocator>,
    raw: &str,
) -> ApiResult<IpAddr> {
    use zerovpn_wg::ip_alloc::AllocError;
    let parsed: std::net::IpAddr = raw
        .trim()
        .parse()
        .map_err(|_| ApiError::Validation(format!("invalid IP: {raw}")))?;
    match alloc.try_reserve(parsed) {
        Ok(()) => Ok(parsed),
        Err(AllocError::AlreadyAllocated) => Err(ApiError::Conflict(format!(
            "{parsed} is already assigned to another device"
        ))),
        Err(AllocError::OutOfRange) => Err(ApiError::Validation(format!(
            "{parsed} is outside the server's subnet"
        ))),
        Err(AllocError::Reserved) => Err(ApiError::Validation(format!(
            "{parsed} is a reserved address (network / broadcast / gateway)"
        ))),
        Err(AllocError::Exhausted) => {
            Err(ApiError::Conflict("server IP pool exhausted".into()))
        }
        Err(AllocError::FamilyMismatch(want, got)) => Err(ApiError::Validation(format!(
            "IP family mismatch: server expects {want}, got {got}"
        ))),
    }
}
