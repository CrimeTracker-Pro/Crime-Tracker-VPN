//! Host-level metrics emitter for the admin sidebar.
//!
//! Every `TICK` seconds we publish one `Event::ServerHealth` per active
//! server. The numbers are sourced as follows:
//!
//! * **CPU %, memory, Net I/O** — `docker stats` for the VPN host
//!   container (the api/api-dev container that owns `wg0`). Queried over
//!   the local Docker socket so the figures match what the operator sees
//!   from `docker stats <name>` on the host. The container name is taken
//!   from `ZEROVPN_WORKER__VPN_HOST_CONTAINER`; absent → docker socket
//!   missing → falls back to `sysinfo` so the panel is never empty.
//!
//! * **wg0 Real I/O** — per-second rate computed by diffing the cumulative
//!   `rx_bytes`/`tx_bytes` counters on the `wg0` interface against the
//!   previous tick. We try Docker stats' `networks.wg0` first (already
//!   fetched) and fall back to `/sys/class/net/wg0/statistics/{rx,tx}_bytes`
//!   so this works even when running outside Docker, as long as the
//!   process can see the wg0 interface in its netns.
//!
//! All numbers on the wire are rates (per-second) and the diffing /
//! previous-state bookkeeping happens here — consumers just plot the
//! latest value.

use std::time::{Duration, Instant};

use sysinfo::System;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zerovpn_db::{PgPool, repos::servers};
use zerovpn_wire::Event;

use crate::docker_stats;

const TICK: Duration = Duration::from_secs(5);

/// Cumulative byte counters carried across ticks so we can compute a
/// per-second rate by diffing. `None` until the first read populates it.
#[derive(Default)]
struct ByteCounters {
    rx: u64,
    tx: u64,
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

pub async fn run(pool: PgPool, tx: mpsc::Sender<(String, Event)>) {
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
    // rate ("Real I/O"). Net I/O is emitted as the raw cumulative total
    // from Docker stats — matches the "Net I/O" column in `docker stats`
    // and doesn't need a previous-tick sample.
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

        // ── Net I/O (cumulative totals, no rate) ─────────────────────
        // Mirror the "Net I/O" column from `docker stats <name>` 1:1:
        // sum across every interface visible in the container (wg0
        // included), and ship the raw cumulative-since-container-start
        // figure. The sidebar formats it as `↓ 39.4 MB · ↑ 13.1 MB`
        // to match what an operator sees on the host.
        let (net_rx_total, net_tx_total) = if let Some(d) = &docker {
            d.net_io_total()
        } else {
            // No docker stats → unknown. Reporting 0 is the truthful
            // "we couldn't measure this" signal; the sidebar's loading
            // text already covers the first-paint moment.
            (0, 0)
        };

        // ── wg0 Real I/O ──────────────────────────────────────────────
        // Prefer Docker's `networks.wg0` (already fetched, no extra
        // syscall). Fall back to reading sysfs directly — works when
        // running outside docker as long as wg0 is in our netns.
        let wg_cum = docker
            .as_ref()
            .and_then(|d| d.networks.get("wg0").map(|n| (n.rx_bytes, n.tx_bytes)))
            .or_else(|| read_iface_counters("wg0"));
        let (wg_rx_bps, wg_tx_bps) = match (wg_cum, prev_wg.as_ref()) {
            (Some((rx, tx)), Some(p)) => {
                (rate_per_sec(p.rx, rx, secs), rate_per_sec(p.tx, tx, secs))
            }
            (Some(_), None) => (0, 0),
            (None, _) => (0, 0),
        };
        if let Some((rx, tx)) = wg_cum {
            prev_wg = Some(ByteCounters { rx, tx });
        }

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
