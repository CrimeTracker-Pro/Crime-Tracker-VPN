//! Shared wire schema for ZMQ pub/sub and WebSocket frames.
//!
//! Compiled to WASM for the frontend so backend and frontend share one source
//! of truth for message types. Use MessagePack (rmp-serde) on the wire.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Single envelope type for everything the worker publishes to the API and
/// the API forwards to the browser. Adding a new variant requires both ends
/// to be redeployed; never reuse a discriminant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Heartbeat from the worker; lets the API confirm the bus is alive.
    Heartbeat { ts_ms: i64 },

    /// Per-peer bandwidth delta (bytes since previous sample).
    StatsDelta {
        device_id: Uuid,
        user_id: Uuid,
        rx_bytes: u64,
        tx_bytes: u64,
        rate_rx_bps: u64,
        rate_tx_bps: u64,
        /// Authoritative cumulative lifetime totals for this device, after
        /// folding in this tick's delta. Lets the client display a "Total"
        /// that grows in real time and matches the server exactly, instead
        /// of accumulating deltas locally (which drifts on dropped frames).
        total_rx_bytes: u64,
        total_tx_bytes: u64,
        ts_ms: i64,
    },

    /// A peer's last_handshake timestamp moved.
    HandshakeChange {
        device_id: Uuid,
        user_id: Uuid,
        last_handshake_ms: i64,
    },

    /// A peer's status changed (paused/unpaused/revoked).
    PeerStatusChanged {
        device_id: Uuid,
        user_id: Uuid,
        status: PeerStatus,
    },

    /// Server-wide health snapshot. Emitted by the worker every 5 s for
    /// admin dashboards. CPU%, memory, wg0 tunnel I/O ("Real I/O"),
    /// container network I/O ("Net I/O"), uptime, peer count. CPU/memory/
    /// net come from the Docker engine's `/containers/{name}/stats` API
    /// for the VPN host container (so the numbers match `docker stats`).
    /// `wg_rx_bps` / `wg_tx_bps` are the per-second rate on the `wg0`
    /// tunnel inside that container, derived by diffing cumulative byte
    /// counters between successive polls. All rates are per-second.
    ServerHealth {
        server_id: Uuid,
        cpu_pct: f32,
        mem_used_bytes: u64,
        mem_total_bytes: u64,
        active_peers: u32,
        /// wg0 tunnel: bytes received per second. Diff of the cumulative
        /// counter (`/sys/class/net/wg0/statistics/rx_bytes` or the same
        /// number from Docker stats `networks.wg0`) since the previous
        /// poll. Surfaced in the sidebar as "Real I/O".
        wg_rx_bps: u64,
        /// wg0 tunnel: bytes transmitted per second.
        wg_tx_bps: u64,
        /// Container Net I/O — **cumulative** bytes received since the
        /// VPN host container started, summed across every interface
        /// (eth0 + wg0 + …). Mirrors the "Net I/O" column from
        /// `docker stats <name>` exactly, which is what the sidebar
        /// renders. Use the diff against the previous tick if you need
        /// a per-second rate.
        net_rx_total_bytes: u64,
        /// Container Net I/O — cumulative bytes transmitted, same shape.
        net_tx_total_bytes: u64,
        /// Process uptime in seconds (the worker's own — used in the
        /// "uptime dd hh mm ss" sidebar label).
        uptime_sec: u64,
        ts_ms: i64,
    },

    /// Per-server bandwidth + peer-count tick. Emitted once per poll
    /// round alongside the per-peer `StatsDelta`s. Backed by the
    /// `server_samples` table (migration 5).
    ServerSample {
        server_id: Uuid,
        total_rx_bytes: u64,
        total_tx_bytes: u64,
        rate_rx_bps: u64,
        rate_tx_bps: u64,
        peer_count: u32,
        online_count: u32,
        handshake_count: u32,
        ts_ms: i64,
    },

    /// A mutation happened to persisted data — emitted by the **API** (not
    /// the worker) straight onto the broadcast bus so every other session of
    /// the same user, plus any admin watching, can invalidate the relevant
    /// query and reflect the change in real time. The client maps
    /// `resource` to the cache keys it must refresh and may surface a toast
    /// keyed on `action`.
    ///
    /// `user_id` is the owning user for user-scoped resources (device, user
    /// account); `None` marks admin-global resources (server, maintenance)
    /// that only admins should react to. `id` is the affected row's id when
    /// a single row changed (`None` for bulk operations like reorder).
    DataChanged {
        user_id: Option<Uuid>,
        resource: ResourceKind,
        id: Option<Uuid>,
        action: ChangeAction,
    },

    /// A ready-to-display notification. The server decides *what* to tell the
    /// user (connectivity changes, quota warnings, security alerts) and the
    /// client just renders it — an in-app toast plus, when the tab is hidden
    /// and the user opted in, an OS notification. Keeps notification copy and
    /// routing server-side so we don't grow a wire variant per category.
    ///
    /// `user_id` scopes delivery (the owning user's sessions + admins);
    /// `None` is admin-only. `url` is an in-app path to open on click; `tag`
    /// de-dupes/replaces a prior notification with the same id.
    Notify {
        user_id: Option<Uuid>,
        level: NotifyLevel,
        title: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        tag: Option<String>,
    },
}

/// Severity/affordance for a [`Event::Notify`] — maps to the client's toast
/// variants.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// What kind of persisted resource a [`Event::DataChanged`] refers to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Device,
    User,
    Server,
    Maintenance,
}

/// The mutation that produced a [`Event::DataChanged`]. Used purely on the
/// client for optional toasts; the cache invalidation only keys on
/// `resource`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeAction {
    Created,
    Updated,
    Deleted,
    Paused,
    Unpaused,
    KeysRotated,
    Reordered,
    Connected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    Active,
    Paused,
    Revoked,
}

/// MessagePack-encode an event.
///
/// Two non-defaults are set on the rmp-serde serializer:
///   - `with_struct_map()` — equivalent to the old `to_vec_named`: encode
///     struct fields by name (a map), not positionally (an array). Lets
///     the JS side decode without a shared schema.
///   - `with_human_readable()` — tells UUID / IpAddr-style types whose
///     serde impl branches on `is_human_readable()` to emit strings, not
///     raw bytes. Without this, `Uuid` lands on the wire as 16 raw bytes
///     and JS sees a Uint8Array (which stringifies to "1,158,0,168…").
pub fn encode(event: &Event) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut buf = Vec::new();
    let mut ser = rmp_serde::Serializer::new(&mut buf)
        .with_struct_map()
        .with_human_readable();
    event.serialize(&mut ser)?;
    Ok(buf)
}

/// MessagePack-decode an event. Mirrors the human-readable flag set on
/// the encode side so a round-trip in Rust still works.
pub fn decode(bytes: &[u8]) -> Result<Event, rmp_serde::decode::Error> {
    let mut de = rmp_serde::Deserializer::new(bytes).with_human_readable();
    Event::deserialize(&mut de)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_roundtrip() {
        let e = Event::Heartbeat { ts_ms: 12345 };
        let bytes = encode(&e).unwrap();
        let decoded = decode(&bytes).unwrap();
        match decoded {
            Event::Heartbeat { ts_ms } => assert_eq!(ts_ms, 12345),
            _ => panic!("wrong variant"),
        }
    }
}
