//! Active connection flows (`conntrack` view).
//!
//! Ported from the legacy VPN project's "Connections" feature: surface the
//! live `ESTABLISHED` flows the kernel knows about so the topology view
//! can render "who's talking to what right now". Two endpoints expose the
//! same shape:
//!
//! - `GET /connections` — user-scoped. Only flows where the source IP or
//!   destination IP is one of the caller's own device peer-IPs.
//! - `GET /admin/connections` — admin-only. Every observed flow against
//!   any known peer (foreign IPs are still surfaced as `External`).
//!
//! ## Sources
//! 1. Live conntrack (`conntrack -L`). This is what the legacy backend
//!    used and it's the most truthful read — it returns whatever the
//!    kernel is tracking *right now*. Requires the `conntrack` binary
//!    and access to the wg netns (api-dev / api owns wg0, so it runs
//!    there natively).
//! 2. `/proc/net/nf_conntrack`. Same data, no external binary. Tried if
//!    `conntrack -L` failed.
//! 3. `destination_ips` table fallback. The worker's flow ingester
//!    already persists destinations there; if both kernel sources are
//!    unavailable we serve the last 60 s of persisted rows so the
//!    topology stays populated.
//!
//! Each tier returns the same `Flow` shape so the API contract doesn't
//! change with the active source. The frontend polls every 3 s.

use axum::{Json, extract::State, response::IntoResponse};
use ipnetwork::IpNetwork;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use time::{Duration, OffsetDateTime};
use tracing::{debug, warn};
use utoipa::ToSchema;
use uuid::Uuid;
use zerovpn_core::models::Device;
use zerovpn_db::repos::devices;

use crate::{
    error::ApiResult,
    extractors::auth::{CurrentUser, RequireAdmin},
    state::AppState,
};

/// One side of a flow. `device_id` / `user_id` / `name` are present when
/// the IP matched a known peer; otherwise it's a foreign endpoint
/// (Internet target, NAT gateway, …) and only `ip` is set with `name =
/// "External"`.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct Endpoint {
    pub ip: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
}

/// One conntrack flow row, rendered the way the frontend expects.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct Flow {
    pub source: Endpoint,
    pub target: Endpoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_port: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_port: Option<i32>,
    /// Lower-case protocol — typically `tcp`, `udp`, `icmp`, …
    pub protocol: String,
}

#[utoipa::path(
    get,
    path = "/connections",
    tag = "Connections",
    responses(
        (status = 200, description = "Active flows touching one of the caller's devices", body = Vec<Flow>),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_for_user(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let all = collect_flows(&state).await;
    let own_ips: HashSet<String> = devices::list_for_user(&state.pool, user.id)
        .await
        .unwrap_or_default()
        .iter()
        .map(|d| d.allocated_ip.ip().to_string())
        .collect();
    // Keep only flows that touch one of the caller's own peers, then mask the
    // identity of the *other* side. AllowedIPs is a split-tunnel to the whole
    // VPN subnet, so a peer↔peer flow can cross another user's device; a
    // non-admin must never learn that peer's device name / device_id / user_id
    // just because the flow touched their own device. The IP is retained (it's
    // the far end of the caller's own traffic) but rendered as an anonymous
    // "Peer". Own devices and genuine `External` endpoints are untouched.
    let filtered: Vec<Flow> = all
        .into_iter()
        .filter(|f| own_ips.contains(&f.source.ip) || own_ips.contains(&f.target.ip))
        .map(|mut f| {
            for ep in [&mut f.source, &mut f.target] {
                if !own_ips.contains(&ep.ip) && ep.device_id.is_some() {
                    ep.name = "Peer".to_string();
                    ep.device_id = None;
                    ep.user_id = None;
                }
            }
            f
        })
        .collect();
    Ok(Json(filtered))
}

#[utoipa::path(
    get,
    path = "/admin/connections",
    tag = "Admin",
    responses(
        (status = 200, description = "Every observed flow against a known peer (foreign IPs included as `External`)", body = Vec<Flow>),
        (status = 403, description = "Not an admin"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_all(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> ApiResult<impl IntoResponse> {
    // Only surface flows that touch a known VPN peer on at least one
    // side. Without this every Docker bridge / postgres
    // conntrack entry leaks into the topology as `External` ↔
    // `External` noise — which is exactly what makes the graph
    // unreadable. Peer↔external (peer browsing the internet) and the
    // rare peer↔peer flows are kept.
    let flows: Vec<Flow> = collect_flows(&state)
        .await
        .into_iter()
        .filter(|f| f.source.device_id.is_some() || f.target.device_id.is_some())
        .collect();
    Ok(Json(flows))
}

/// Try the live conntrack sources first, fall back to the persisted
/// destination_ips rows. Always returns a vec — never errors out — so a
/// missing source doesn't 500 the topology view.
async fn collect_flows(state: &AppState) -> Vec<Flow> {
    // Build the IP→peer map once per request and reuse it for whichever
    // source ends up serving.
    let devices_all = devices::list_all_active(&state.pool).await.unwrap_or_default();
    let ip_to_peer = build_ip_index(&devices_all);
    let to_endpoint = |ip: &str| endpoint_for(ip, &ip_to_peer);

    if let Some(rows) = read_conntrack_binary().await {
        let flows = parse_conntrack(&rows, &to_endpoint);
        if !flows.is_empty() {
            debug!(count = flows.len(), "conntrack -L: served flows");
            return flows;
        }
    }
    if let Some(rows) = read_conntrack_procfs().await {
        let flows = parse_conntrack(&rows, &to_endpoint);
        if !flows.is_empty() {
            debug!(count = flows.len(), "/proc/net/nf_conntrack: served flows");
            return flows;
        }
    }
    let flows = read_destination_ips_fallback(state, &to_endpoint).await;
    if !flows.is_empty() {
        debug!(count = flows.len(), "destination_ips fallback: served flows");
    }
    flows
}

// ── IP / peer mapping ──────────────────────────────────────────────────

struct PeerInfo {
    device_id: Uuid,
    user_id: Uuid,
    name: String,
}

fn build_ip_index(devices_all: &[Device]) -> HashMap<String, PeerInfo> {
    let mut m = HashMap::with_capacity(devices_all.len());
    for d in devices_all {
        m.insert(
            d.allocated_ip.ip().to_string(),
            PeerInfo {
                device_id: d.id,
                user_id: d.user_id,
                name: d.name.clone(),
            },
        );
    }
    m
}

fn endpoint_for(
    ip: &str,
    ip_to_peer: &HashMap<String, PeerInfo>,
) -> Endpoint {
    if let Some(peer) = ip_to_peer.get(ip) {
        Endpoint {
            ip: ip.to_string(),
            name: peer.name.clone(),
            device_id: Some(peer.device_id),
            user_id: Some(peer.user_id),
        }
    } else {
        Endpoint {
            ip: ip.to_string(),
            name: "External".to_string(),
            device_id: None,
            user_id: None,
        }
    }
}

// ── conntrack source: binary ───────────────────────────────────────────

async fn read_conntrack_binary() -> Option<String> {
    let out = tokio::process::Command::new("conntrack")
        .arg("-L")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

// ── conntrack source: procfs ───────────────────────────────────────────

async fn read_conntrack_procfs() -> Option<String> {
    tokio::fs::read_to_string("/proc/net/nf_conntrack").await.ok()
}

// ── conntrack parser ───────────────────────────────────────────────────
//
// `conntrack -L` output (kernel docs):
//   tcp      6 431999 ESTABLISHED src=10.10.0.2 dst=1.1.1.1 sport=5678 dport=80 ...
//
// `/proc/net/nf_conntrack` is the same data, prefixed by the L3 family:
//   ipv4     2 tcp      6 431999 ESTABLISHED src=10.10.0.2 dst=1.1.1.1 ...
//
// Either way, we walk `src=` / `dst=` / `sport=` / `dport=` tokens and
// take the first occurrence (which is the original direction).

fn parse_conntrack<F>(text: &str, mk_endpoint: &F) -> Vec<Flow>
where
    F: Fn(&str) -> Endpoint,
{
    let mut flows = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in text.lines() {
        if !line.contains("ESTABLISHED") {
            continue;
        }
        // The protocol token is one of the first 1-2 cols depending on
        // whether the line has the `ipv4 2` L3 prefix. Take whichever
        // matches a known L4 protocol.
        let parts: Vec<&str> = line.split_whitespace().collect();
        let proto = parts
            .iter()
            .find(|p| matches!(**p, "tcp" | "udp" | "icmp" | "icmpv6" | "sctp" | "dccp"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "tcp".to_string());

        let mut src = None;
        let mut dst = None;
        let mut sport = None;
        let mut dport = None;
        for part in &parts {
            if let Some(rest) = part.strip_prefix("src=") {
                if src.is_none() {
                    src = Some(rest.to_string());
                }
            } else if let Some(rest) = part.strip_prefix("dst=") {
                if dst.is_none() {
                    dst = Some(rest.to_string());
                }
            } else if let Some(rest) = part.strip_prefix("sport=") {
                if sport.is_none() {
                    sport = rest.parse::<i32>().ok();
                }
            } else if let Some(rest) = part.strip_prefix("dport=")
                && dport.is_none() {
                    dport = rest.parse::<i32>().ok();
                }
        }

        let (Some(src_ip), Some(dst_ip)) = (src, dst) else {
            continue;
        };
        // Hide self-loops on the WG hub: src == dst happens on local
        // listener entries (sshd binding 0.0.0.0, etc) and isn't a flow
        // anyone wants to render.
        if src_ip == dst_ip {
            continue;
        }
        let key = format!("{proto}:{src_ip}:{sport:?}->{dst_ip}:{dport:?}");
        if !seen.insert(key) {
            continue;
        }
        flows.push(Flow {
            source: mk_endpoint(&src_ip),
            target: mk_endpoint(&dst_ip),
            source_port: sport,
            target_port: dport,
            protocol: proto,
        });
    }
    flows
}

// ── destination_ips fallback ───────────────────────────────────────────
//
// When conntrack is unavailable (binary missing, no permission, or running
// somewhere without `/proc/net/nf_conntrack`), serve the last minute of
// the worker's ingested flows so the topology still has something to
// render. We coalesce identical 5-tuples so a burst of repeats doesn't
// crowd the graph.

async fn read_destination_ips_fallback<F>(state: &AppState, mk_endpoint: &F) -> Vec<Flow>
where
    F: Fn(&str) -> Endpoint,
{
    let since = OffsetDateTime::now_utc() - Duration::seconds(60);
    type FlowRow = (String, Option<i32>, String, Option<i32>, Option<String>);
    let rows: Vec<FlowRow> = match sqlx::query_as(
        r#"SELECT DISTINCT ON (src_ip, src_port, dst_ip, dst_port, proto)
                  src_ip, src_port, dst_ip, dst_port, proto
             FROM destination_ips
            WHERE started_at >= $1
            ORDER BY src_ip, src_port, dst_ip, dst_port, proto, started_at DESC
            LIMIT 200"#,
    )
    .bind(since)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rs) => rs,
        Err(e) => {
            warn!(?e, "destination_ips fallback query failed");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter(|(src, _, dst, _, _)| src != dst)
        .map(|(src, sport, dst, dport, proto)| Flow {
            source: mk_endpoint(&src),
            target: mk_endpoint(&dst),
            source_port: sport,
            target_port: dport,
            protocol: proto.unwrap_or_else(|| "tcp".to_string()),
        })
        .collect()
}

#[allow(dead_code)]
fn _force_ipnetwork_use(_n: IpNetwork) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn external(ip: &str) -> Endpoint {
        Endpoint {
            ip: ip.to_string(),
            name: "External".to_string(),
            device_id: None,
            user_id: None,
        }
    }

    #[test]
    fn parses_conntrack_binary_output() {
        let sample = "tcp      6 431999 ESTABLISHED src=10.10.0.2 dst=1.1.1.1 sport=5678 dport=443 \
                      src=1.1.1.1 dst=10.10.0.2 sport=443 dport=5678 [ASSURED] mark=0 use=1\n\
                      udp      17 29 src=10.10.0.3 dst=8.8.8.8 sport=12345 dport=53 [UNREPLIED] ESTABLISHED";
        let flows = parse_conntrack(sample, &|ip: &str| external(ip));
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0].source.ip, "10.10.0.2");
        assert_eq!(flows[0].target.ip, "1.1.1.1");
        assert_eq!(flows[0].source_port, Some(5678));
        assert_eq!(flows[0].target_port, Some(443));
        assert_eq!(flows[0].protocol, "tcp");
        assert_eq!(flows[1].protocol, "udp");
    }

    #[test]
    fn parses_procfs_output_with_l3_prefix() {
        // /proc/net/nf_conntrack prepends `ipv4 2` before the L4 cols.
        let sample = "ipv4     2 tcp      6 431999 ESTABLISHED src=10.10.0.5 dst=93.184.216.34 sport=44321 dport=80 \
                      src=93.184.216.34 dst=10.10.0.5 sport=80 dport=44321 [ASSURED] mark=0";
        let flows = parse_conntrack(sample, &|ip: &str| external(ip));
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].protocol, "tcp");
        assert_eq!(flows[0].source.ip, "10.10.0.5");
        assert_eq!(flows[0].source_port, Some(44321));
    }

    #[test]
    fn dedupes_repeated_five_tuples() {
        let line =
            "tcp      6 431999 ESTABLISHED src=10.10.0.2 dst=1.1.1.1 sport=5678 dport=443\n";
        let sample = format!("{line}{line}{line}");
        let flows = parse_conntrack(&sample, &|ip: &str| external(ip));
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn drops_self_loops() {
        let sample =
            "tcp      6 431999 ESTABLISHED src=127.0.0.1 dst=127.0.0.1 sport=22 dport=22";
        let flows = parse_conntrack(sample, &|ip: &str| external(ip));
        assert!(flows.is_empty());
    }
}
