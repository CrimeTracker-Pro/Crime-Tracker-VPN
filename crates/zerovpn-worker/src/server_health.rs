//! Host-level metrics emitter for the admin sidebar.
//!
//! Every `TICK` seconds we publish one `Event::ServerHealth` per active
//! server. The numbers are sourced as follows:
//!
//! * **CPU %, memory** — `docker stats` for the VPN host
//!   container (the api/api-dev container that owns `wg0`). Queried over
//!   the local Docker socket so the figures match what the operator sees
//!   from `docker stats <name>` on the host. The container name is taken
//!   from `ZEROVPN_WORKER__VPN_HOST_CONTAINER`; absent → docker socket
//!   missing → falls back to `sysinfo` so the panel is never empty.
//!
//! * **VPN total** — durable host-perspective peer RX/TX from PostgreSQL.
//!   This survives API and worker container recreation. It is not Docker's
//!   container network I/O or the raw wg0 interface packet counter.
//!
//! * **wg0 Real I/O** — per-second rate from cumulative interface counters.
//!   In host-agent mode the agent reads host `wg0` and the rate baseline
//!   resets on interface generation changes or missing samples. Legacy mode
//!   uses Docker stats' `networks.wg0`, then local sysfs as a fallback.
//!
//! Only wg0 Real I/O is a rate; VPN total is cumulative.

use std::time::{Duration, Instant};

use sysinfo::System;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zerovpn_db::{PgPool, repos::{peer_usage_checkpoints, servers}};
use zerovpn_wire::Event;

use crate::docker_stats;
use crate::wg_source::StatsSource;

const TICK: Duration = Duration::from_secs(5);

/// Cumulative byte counters carried across ticks so we can compute a
/// per-second rate by diffing. `None` until the first read populates it.
#[derive(Default)]
struct ByteCounters {
    rx: u64,
    tx: u64,
    generation: Option<String>,
}

/// Read cumulative `rx_bytes` / `tx_bytes` for an interface from sysfs.
/// Falls back to `None` when the path doesn't exist (non-Linux, or the
/// interface isn't present in this netns) so the caller can substitute the
/// Docker-reported counters or report 0.
fn read_iface_counters(iface: &str) -> Option<(u64, u64)> {
    let base = format!("/sys/class/net/{iface}/statistics");
    let rx: u64 = std::fs::read_to_string(format!("{base}/rx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let tx: u64 = std::fs::read_to_string(format!("{base}/tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((rx, tx))
}

/// Compute a per-second byte rate from (prev, cur) cumulative counters.
/// A counter reset (cur < prev) — container restart, interface re-create
/// — is treated as "fresh baseline, no rate this tick" rather than a
/// negative number wrapping around.
fn rate_per_sec(prev: u64, cur: u64, secs: u64) -> u64 {
    if cur < prev || secs == 0 {
        return 0;
    }
    (cur - prev) / secs
}

fn interface_rates(prev: Option<&ByteCounters>, cur: Option<&ByteCounters>, secs: u64) -> (u64, u64) {
    match (prev, cur) {
        (Some(prev), Some(cur)) if prev.generation == cur.generation => (
            rate_per_sec(prev.rx, cur.rx, secs),
            rate_per_sec(prev.tx, cur.tx, secs),
        ),
        _ => (0, 0),
    }
}

pub async fn run(pool: PgPool, tx: mpsc::Sender<(String, Event)>, mut host_source: Option<StatsSource>) {
    info!(?TICK, "server_health emitter started");
    let started = Instant::now();

    // Name of the container whose CPU/MEM/Net we want to report. In dev
    // compose this is `crimetracker-vpn-api-dev`; the main compose names it
    // `crimetracker-vpn-api`. Empty / unset → docker stats disabled, fall back
    // to sysinfo.
    let target_container = std::env::var("ZEROVPN_WORKER__VPN_HOST_CONTAINER")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(name) = &target_container {
        info!(container = %name, "server_health: using docker stats for CPU/MEM/Net");
    } else {
        info!(
            "server_health: ZEROVPN_WORKER__VPN_HOST_CONTAINER not set; \
             falling back to host-wide sysinfo for CPU/MEM/Net"
        );
    }

    // Fallback path: prime sysinfo so its first non-noise sample arrives
    // on the next refresh. Only used when docker stats are unavailable.
    let mut sys = System::new_all();
    sys.refresh_cpu_usage();
    sys.refresh_memory();

    // wg0 byte counters are diffed across ticks to compute a per-second
    // rate ("Real I/O"). The persistent VPN total is read from Postgres.
    let mut prev_wg: Option<ByteCounters> = None;

    let mut ticker = tokio::time::interval(TICK);
    ticker.tick().await; // first tick is immediate; skip it.

    loop {
        ticker.tick().await;
        let secs = TICK.as_secs().max(1);

        // ── Pull stats from Docker if configured ──────────────────────
        let docker = if let Some(name) = &target_container {
            match docker_stats::fetch(name).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(?e, container = %name, "docker stats fetch failed");
                    None
                }
            }
        } else {
            None
        };

        // ── CPU / Memory ──────────────────────────────────────────────
        let (cpu_pct, mem_used_bytes, mem_total_bytes) = if let Some(d) = &docker {
            (d.cpu_pct(), d.mem_used_real(), d.mem_limit())
        } else {
            // sysinfo fallback (host-wide, not container-scoped).
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            let cpus = sys.cpus();
            let pct = if cpus.is_empty() {
                0.0
            } else {
                cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32
            };
            (pct, sys.used_memory(), sys.total_memory())
        };

        // ── wg0 Real I/O ──────────────────────────────────────────────
        // Host mode reads the host interface through the agent only. Legacy
        // mode prefers Docker's networks.wg0, then local sysfs.
        let wg_cum = if let Some(source) = host_source.as_mut() {
            match source.snapshot().await {
                Ok(snapshot) => snapshot.interface_counters.map(|(rx, tx)|
                    (rx, tx, Some(snapshot.source_generation))),
                Err(e) => {
                    warn!(?e, "host-agent interface counters unavailable");
                    None
                }
            }
        } else {
            docker.as_ref()
                .and_then(|d| d.networks.get("wg0").map(|n| (n.rx_bytes, n.tx_bytes)))
                .or_else(|| read_iface_counters("wg0"))
                .map(|(rx, tx)| (rx, tx, None))
        };
        let current_wg = wg_cum.map(|(rx, tx, generation)| ByteCounters { rx, tx, generation });
        let (wg_rx_bps, wg_tx_bps) = interface_rates(prev_wg.as_ref(), current_wg.as_ref(), secs);
        prev_wg = current_wg;

        let uptime_sec = started.elapsed().as_secs();
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;

        // Emit one event per active server. The current v1 model has a
        // single server per deployment, but this loop is forward-compatible.
        let active_servers = match servers::list_active(&pool).await {
            Ok(s) => s,
            Err(e) => {
                warn!(?e, "server_health: list_active failed");
                continue;
            }
        };
        for s in active_servers {
            let (net_rx_total, net_tx_total) =
                match peer_usage_checkpoints::server_totals(&pool, s.id).await {
                    Ok((rx, tx)) => (rx.max(0) as u64, tx.max(0) as u64),
                    Err(e) => {
                        warn!(?e, server = %s.id, "VPN total query failed");
                        continue;
                    }
                };
            let active_peers =
                zerovpn_db::repos::devices::count_active_for_server(&pool, s.id)
                    .await
                    .unwrap_or(0) as u32;
            let event = Event::ServerHealth {
                server_id: s.id,
                cpu_pct,
                mem_used_bytes,
                mem_total_bytes,
                active_peers,
                wg_rx_bps,
                wg_tx_bps,
                net_rx_total_bytes: net_rx_total,
                net_tx_total_bytes: net_tx_total,
                uptime_sec,
                ts_ms: now_ms,
            };
            debug!(
                server = %s.id,
                cpu_pct,
                mem_used_bytes,
                mem_total_bytes,
                wg_rx_bps,
                wg_tx_bps,
                net_rx_total = net_rx_total,
                net_tx_total = net_tx_total,
                uptime_sec,
                active_peers,
                "server_health emit"
            );
            let topic = format!("events.server.{}", s.id);
            if tx.send((topic, event)).await.is_err() {
                debug!("server_health: channel closed, exiting");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_io_resets_on_generation_change_or_missing_sample() {
        let first = ByteCounters { rx: 100, tx: 200, generation: Some("host:first".into()) };
        let next = ByteCounters { rx: 150, tx: 230, generation: Some("host:first".into()) };
        let recreated = ByteCounters { rx: 5, tx: 3, generation: Some("host:second".into()) };
        assert_eq!(interface_rates(Some(&first), Some(&next), 5), (10, 6));
        assert_eq!(interface_rates(Some(&next), Some(&recreated), 5), (0, 0));
        assert_eq!(interface_rates(None, Some(&next), 5), (0, 0));
        assert_eq!(interface_rates(Some(&next), None, 5), (0, 0));
    }
}
