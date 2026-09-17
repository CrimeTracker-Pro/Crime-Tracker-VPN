//! Real WG stats poller.
//!
//! When `ZEROVPN_WG__BACKEND` is a real backend (`shell` or `kernel`) AND
//! the `wg` binary is in PATH, this task polls `wg show <iface> dump` every
//! `ZEROVPN_STATS_INTERVAL_SECS` and emits `Event::StatsDelta` per peer,
//! while persisting endpoints, connection sessions, handshakes, bandwidth
//! samples and server samples. In `noop` mode (dev/macOS, no interface)
//! the poller doesn't run and none of this is captured.
//!
//! `wg show <iface> dump` columns (per peer):
//!   public_key  preshared_key  endpoint  allowed_ips  latest_handshake  rx_bytes  tx_bytes  persistent_keepalive
//!
//! Cumulative counters; we keep an in-memory map of last-seen values per
//! public key and emit deltas. Resets (rx/tx going backward) reset the
//! baseline.

use std::{collections::HashMap, time::Duration};

use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;
use zerovpn_db::{
    PgPool,
    repos::{
        audit, bandwidth, candles, connection_sessions, devices, peer_endpoint_history,
        server_samples, servers, users,
    },
};
use zerovpn_db::repos::candles::CandleRow;
use zerovpn_wire::{Event, NotifyLevel};

/// How long a peer can go without us seeing inbound bytes before we treat it
/// as offline. Peer configs ship `PersistentKeepalive = 30s`, so a connected
/// peer's rx advances at least that often even when idle; ~3 missed keepalives
/// (90s) is a robust drop signal that's far quicker than waiting out the
/// ~2-min WireGuard rekey/handshake. WG peers behind NAT can't be reliably
/// probed from the server, so this keepalive-driven liveness is the fast,
/// correct substitute for a server-initiated heartbeat. Kept in sync with the
/// frontend's `ACTIVITY_STALE_MS`.
const OFFLINE_AFTER_SECS: i64 = 90;
/// Upper bound: a handshake older than this is always offline regardless of
/// activity (matches the previous behaviour / the frontend's connState).
const HANDSHAKE_STALE_SECS: i64 = 180;

/// Per-pubkey in-memory state between poll ticks. Holds the cumulative
/// rx/tx counters (so we can emit deltas) and the last-observed
/// endpoint (so we only hit `devices` / `peer_endpoint_history` when it
/// actually changes — per-tick polling at 1 s with hundreds of peers
/// would otherwise hammer the DB pointlessly).
#[derive(Default, Clone)]
struct Cumulative {
    rx: u64,
    tx: u64,
    endpoint: Option<String>,
    /// Last-seen `latest_handshake` (unix seconds). Lets us emit a
    /// `HandshakeChange` only when the timestamp actually advances, instead
    /// of every tick — that event invalidates the device query on the
    /// frontend so the online/offline pill flips without a manual refresh.
    handshake: i64,
}

/// One peer's (or one server's) in-progress 1-minute candle. Each per-second
/// tick folds a rate (bits/sec) into High/Low/Σ/samples in O(1); on the
/// minute boundary the bar becomes a flushed `CandleRow`.
#[derive(Default, Clone)]
struct Bar {
    rx_high: i64,
    rx_low: i64,
    rx_sum: i64,
    tx_high: i64,
    tx_low: i64,
    tx_sum: i64,
    samples: i32,
}

impl Bar {
    fn observe(&mut self, rx_bps: i64, tx_bps: i64) {
        if self.samples == 0 {
            self.rx_high = rx_bps;
            self.rx_low = rx_bps;
            self.tx_high = tx_bps;
            self.tx_low = tx_bps;
        } else {
            self.rx_high = self.rx_high.max(rx_bps);
            self.rx_low = self.rx_low.min(rx_bps);
            self.tx_high = self.tx_high.max(tx_bps);
            self.tx_low = self.tx_low.min(tx_bps);
        }
        self.rx_sum += rx_bps;
        self.tx_sum += tx_bps;
        self.samples += 1;
    }

    /// Skip flushing minutes that were idle end-to-end — for hundreds of
    /// peers this avoids writing a zero-row per peer per minute forever.
    /// Idle stretches simply show as gaps in the candle chart.
    fn has_traffic(&self) -> bool {
        self.rx_sum > 0 || self.tx_sum > 0
    }
}

/// In-memory OHLC accumulator shared across poll ticks. Holds the bars for
/// the minute currently being filled (per device and per server); rolling to
/// a new minute drains the completed bars for the worker to flush.
#[derive(Default)]
struct CandleAccumulator {
    /// Minute bucket (floored UTC) the live bars belong to. `None` until the
    /// first tick after boot.
    minute: Option<time::OffsetDateTime>,
    devices: HashMap<Uuid, Bar>,
    servers: HashMap<Uuid, Bar>,
}

impl CandleAccumulator {
    /// Move the accumulator onto `bucket`. If a *different* minute was in
    /// progress, drain it and return its completed (device_rows, server_rows)
    /// for flushing. Idle bars are filtered out.
    fn roll_to(&mut self, bucket: time::OffsetDateTime) -> Option<(Vec<CandleRow>, Vec<CandleRow>)> {
        match self.minute {
            Some(prev) if prev != bucket => {
                let dev = drain_bars(&mut self.devices, prev);
                let srv = drain_bars(&mut self.servers, prev);
                self.minute = Some(bucket);
                Some((dev, srv))
            }
            None => {
                self.minute = Some(bucket);
                None
            }
            _ => None,
        }
    }
}

/// Drain a bar map into flushable rows, dropping idle bars and resetting the
/// map for the next minute.
fn drain_bars(map: &mut HashMap<Uuid, Bar>, bucket: time::OffsetDateTime) -> Vec<CandleRow> {
    map.drain()
        .filter(|(_, b)| b.has_traffic())
        .map(|(id, b)| CandleRow {
            id,
            bucket_start: bucket,
            rx_high: b.rx_high,
            rx_low: b.rx_low,
            rx_sum: b.rx_sum,
            tx_high: b.tx_high,
            tx_low: b.tx_low,
            tx_sum: b.tx_sum,
            samples: b.samples,
        })
        .collect()
}

/// Floor a timestamp to its minute (UTC) — the candle's `bucket_start`.
fn floor_minute(t: time::OffsetDateTime) -> time::OffsetDateTime {
    let secs = t.unix_timestamp();
    time::OffsetDateTime::from_unix_timestamp(secs - secs.rem_euclid(60)).unwrap_or(t)
}

pub fn enabled() -> bool {
    // Poll whenever a *real* WG interface exists. Both `shell` and
    // `kernel` backends bring up `wg0`, and `wg show <iface> dump` reads
    // kernel state in either case — so production (kernel) must poll too.
    // Only `noop` (dev/macOS, no interface) has nothing to read.
    std::env::var("ZEROVPN_WG__BACKEND")
        .map(|v| v == "shell" || v == "kernel")
        .unwrap_or(false)
}

fn poll_interval() -> Duration {
    std::env::var("ZEROVPN_STATS_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1))
}

fn interface() -> String {
    std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into())
}

pub async fn run(pool: PgPool, tx: mpsc::Sender<(String, Event)>) {
    let interval = poll_interval();
    let iface = interface();
    info!(?interval, %iface, "real WG poller started");
    let mut last: HashMap<String, Cumulative> = HashMap::new();
    // Last-known cumulative lifetime totals per device, mirrored from the DB
    // `accumulate_lifetime` / `seed_lifetime` writes. Lets every per-peer
    // `StatsDelta` carry the absolute total — including idle ticks where we
    // skip the DB write — so the client's "Total" tracks the server exactly.
    let mut lifetimes: HashMap<Uuid, (u64, u64)> = HashMap::new();
    // Highest monthly-quota tier each user has already been notified about
    // (0 none · 1 ≥90% · 2 ≥100%). Lets us fire the warning/limit notification
    // exactly once per crossing and re-arm when usage resets next month.
    let mut quota_tier: HashMap<Uuid, u8> = HashMap::new();
    // Last unix-second at which we saw inbound bytes (a keepalive or real
    // traffic) from each peer. Drives the fast, keepalive-based offline
    // detection in place of the slow handshake window.
    let mut last_activity: HashMap<Uuid, i64> = HashMap::new();
    // Per-device online flag from the previous tick. Powers transition
    // detection — when the value flips we write an audit_logs row so the
    // device-detail "Activity" timeline can render online/offline
    // events. `None` = "not yet observed this session" (no entry emitted
    // on the very first tick after worker boot, otherwise the timeline
    // would gain a phantom transition for every existing peer).
    let mut prev_online: HashMap<Uuid, bool> = HashMap::new();
    // OHLC accumulator — survives across ticks, flushes one row per
    // device/server per minute.
    let mut candles = CandleAccumulator::default();
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        match poll_once(
            &pool,
            &tx,
            &iface,
            &mut last,
            &mut lifetimes,
            &mut quota_tier,
            &mut last_activity,
            &mut prev_online,
            &mut candles,
            interval,
        )
        .await
        {
            Ok(n) => debug!(peers = n, "wg poll"),
            Err(e) => warn!(?e, "wg poll failed"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn poll_once(
    pool: &PgPool,
    tx: &mpsc::Sender<(String, Event)>,
    iface: &str,
    last: &mut HashMap<String, Cumulative>,
    lifetimes: &mut HashMap<Uuid, (u64, u64)>,
    quota_tier: &mut HashMap<Uuid, u8>,
    last_activity: &mut HashMap<Uuid, i64>,
    prev_online: &mut HashMap<Uuid, bool>,
    candle_acc: &mut CandleAccumulator,
    interval: Duration,
) -> anyhow::Result<usize> {
    // Run `wg show <iface> dump`. Output is tab-separated, one peer per line
    // after a header line containing the interface keys. We skip the first
    // line (interface own keys), then parse the rest.
    let out = tokio::process::Command::new("wg")
        .args(["show", iface, "dump"])
        .output()
        .await?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "wg show failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut peers_seen: usize = 0;
    let now = time::OffsetDateTime::now_utc();
    let now_ms = now.unix_timestamp() * 1000;

    // OHLC candles: if this tick crossed a minute boundary, flush the bars
    // that completed last minute (one row per active device + per server) and
    // start filling the new minute below. Best-effort — a flush error must
    // not stop live stats.
    if let Some((dev_rows, srv_rows)) = candle_acc.roll_to(floor_minute(now)) {
        if !dev_rows.is_empty()
            && let Err(e) = candles::insert_device_candles_1m(pool, &dev_rows).await {
                warn!(?e, n = dev_rows.len(), "device candle flush failed");
            }
        if !srv_rows.is_empty()
            && let Err(e) = candles::insert_server_candles_1m(pool, &srv_rows).await {
                warn!(?e, n = srv_rows.len(), "server candle flush failed");
            }
    }

    // Build a quick lookup of pubkey → (device_id, user_id) so we can
    // attribute each peer line.
    let pk_index = devices::pubkey_index(pool).await?;

    // Per-server rollup. `wg show dump` is per-interface but a single
    // Crime Tracker VPN deployment can map multiple servers onto one interface, so
    // we key by server_id from the pubkey index.
    let mut srv_totals: HashMap<Uuid, (u64, u64, u32, u32, u32)> = HashMap::new();
    let secs = interval.as_secs().max(1);

    let mut lines = stdout.lines();
    let _interface_line = lines.next();
    for line in lines {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 8 {
            continue;
        }
        let public_key = cols[0];
        // cols[1] is the peer's pre-shared key (we don't capture it —
        // it's a secret, not a log target). cols[2] is the public
        // "ip:port" the peer last connected from; "(none)" when the
        // peer hasn't completed a handshake yet.
        let endpoint_raw = cols[2];
        let endpoint_now: Option<String> = if endpoint_raw == "(none)" || endpoint_raw.is_empty() {
            None
        } else {
            Some(endpoint_raw.to_string())
        };
        let latest_handshake: i64 = cols[4].parse().unwrap_or(0);
        let rx_total: u64 = cols[5].parse().unwrap_or(0);
        let tx_total: u64 = cols[6].parse().unwrap_or(0);

        let prev_entry = last.get(public_key).cloned();
        // First sight of this peer in this worker session. We must NOT treat
        // the full cumulative counter as a delta here — on a worker restart
        // that would re-count the peer's entire history (a spike in the
        // chart and a doubled lifetime total). Instead we establish the
        // baseline this tick (delta 0) and reconcile the lifetime against
        // the live counter via `seed_lifetime` below.
        let first_sight = prev_entry.is_none();
        let prev = prev_entry.unwrap_or_default();
        // Counter reset (peer reconnect) → take the new value as the delta.
        let drx = if first_sight {
            0
        } else if rx_total >= prev.rx {
            rx_total - prev.rx
        } else {
            rx_total
        };
        let dtx = if first_sight {
            0
        } else if tx_total >= prev.tx {
            tx_total - prev.tx
        } else {
            tx_total
        };
        let endpoint_changed = endpoint_now.is_some() && endpoint_now != prev.endpoint;
        // A newer handshake than we last saw for this peer — emitted below as
        // a HandshakeChange once we've resolved the peer to a device.
        let handshake_advanced = latest_handshake > prev.handshake;
        last.insert(
            public_key.to_string(),
            Cumulative {
                rx: rx_total,
                tx: tx_total,
                endpoint: endpoint_now.clone(),
                handshake: latest_handshake,
            },
        );

        let Some((device_id, user_id, server_id)) = pk_index.get(public_key).copied() else {
            // Peer present in WG but not in our DB — possibly removed
            // mid-cycle. Skip.
            continue;
        };

        // Persist the endpoint when it changed against our in-memory
        // baseline. Two writes: the latest-only column on `devices`
        // (single-row UPDATE) and an append to `peer_endpoint_history`
        // (so admins can review every distinct endpoint the device has
        // ever connected from). Both are best-effort — a transient DB
        // error here must not stop the live stats broadcast below.
        if endpoint_changed
            && let Some(ref ep) = endpoint_now {
                if let Err(e) =
                    devices::set_last_peer_endpoint(pool, device_id, ep, now).await
                {
                    warn!(?e, %device_id, "set_last_peer_endpoint failed");
                }
                if let Err(e) =
                    peer_endpoint_history::record(pool, device_id, ep, now).await
                {
                    warn!(?e, %device_id, "peer_endpoint_history insert failed");
                }
            }

        // Update last_handshake_at if it changed. Also counts toward the
        // server's per-tick handshake delta (entries that handshook
        // *within this poll interval* — useful for change-rate charts).
        let mut handshake_in_window = false;
        if latest_handshake > 0 {
            let ts = time::OffsetDateTime::from_unix_timestamp(latest_handshake)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
            let _ = devices::touch_handshake(pool, device_id, ts).await;
            let age = now.unix_timestamp() - latest_handshake;
            handshake_in_window = age >= 0 && (age as u64) < secs;

            // Tell the frontend the handshake moved so it re-fetches the
            // device and recomputes the online/offline pill. Only on a real
            // advance (first sight of a handshaked peer, then each WG
            // re-handshake ~every 2 min) so we don't spam an event per tick.
            if handshake_advanced {
                let _ = tx
                    .send((
                        format!("events.peer.{device_id}"),
                        Event::HandshakeChange {
                            device_id,
                            user_id,
                            last_handshake_ms: latest_handshake.saturating_mul(1000),
                        },
                    ))
                    .await;
            }
        }

        // Fold into server rollup: (rx, tx, peers, online, handshakes).
        let entry = srv_totals.entry(server_id).or_default();
        entry.0 = entry.0.saturating_add(drx);
        entry.1 = entry.1.saturating_add(dtx);
        entry.2 += 1; // peer_count
        // online = handshake within last ~180s (WG default keepalive scope).
        // Record inbound activity (a keepalive counts) so we can detect a drop
        // from the *absence* of bytes within OFFLINE_AFTER_SECS — much faster
        // than the handshake window. `drx` is the bytes the peer sent us this
        // tick; with PersistentKeepalive it advances ~every 30s while alive.
        let now_secs = now.unix_timestamp();
        if drx > 0 {
            last_activity.insert(device_id, now_secs);
        }
        // Online = has handshaked AND we've heard from it recently — either a
        // keepalive/traffic within OFFLINE_AFTER_SECS, or a fresh handshake.
        // The handshake also bootstraps liveness right after a worker restart,
        // before the next keepalive lands. Still bounded by the handshake
        // staleness ceiling so a long-dead peer can never read as online.
        let last_act = last_activity.get(&device_id).copied().unwrap_or(0);
        let liveness_ts = latest_handshake.max(last_act);
        let online = latest_handshake > 0
            && (now_secs - liveness_ts) < OFFLINE_AFTER_SECS
            && (now_secs - latest_handshake) < HANDSHAKE_STALE_SECS;
        if online {
            entry.3 += 1;
        }
        if handshake_in_window {
            entry.4 += 1;
        }

        // Online/offline transition → write an audit row. Skip the very
        // first observation per peer this session so existing peers
        // don't generate a phantom transition the moment the worker
        // restarts. We don't suppress this on counter resets — those
        // are real reconnects worth surfacing.
        let prev = prev_online.get(&device_id).copied();
        if let Some(was_online) = prev
            && was_online != online {
                let action = if online {
                    "device.online"
                } else {
                    "device.offline"
                };
                let last_handshake_ms = latest_handshake.saturating_mul(1000);
                if let Err(e) = audit::record(
                    pool,
                    audit::AuditEntry {
                        actor_user_id: None,
                        action,
                        target_type: Some("device"),
                        target_id: Some(device_id),
                        metadata: json!({
                            "last_handshake_ms": last_handshake_ms,
                            "source": "wg_poller",
                        }),
                        ip: None,
                    },
                )
                .await
                {
                    warn!(?e, %device_id, online, "audit record (transition) failed");
                }

                // Live notification to the owner's sessions (+ admins). One
                // name lookup per transition is cheap — transitions are rare.
                let name = devices::get_by_id(pool, device_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.name)
                    .unwrap_or_else(|| "A device".to_string());
                let (title, level) = if online {
                    (format!("{name} came online"), NotifyLevel::Success)
                } else {
                    (format!("{name} went offline"), NotifyLevel::Warning)
                };
                let _ = tx
                    .send((
                        format!("events.user.{user_id}"),
                        Event::Notify {
                            user_id: Some(user_id),
                            level,
                            title,
                            body: None,
                            url: Some(format!("/app/devices/{device_id}")),
                            // Same tag both directions so an offline notice
                            // replaces a stale "came online" and vice versa.
                            tag: Some(format!("conn-{device_id}")),
                        },
                    ))
                    .await;
            }

        // Phase 2 / Stage B — connection_sessions transitions. Unlike
        // the audit row above we DO record the first observation if
        // it's an online state: the startup sweep
        // (`close_all_open`) has just closed any stale rows, so the
        // first-online observation is a real "session start" that the
        // admin connection-history dialog should surface. The audit log
        // stays quiet here to avoid spamming the per-device timeline
        // after every worker restart.

        // ── Device-perspective re-base ──────────────────────────────────
        // `wg show dump` counters are the SERVER's: rx = bytes the server
        // received from the peer (= the device's UPLOAD), tx = bytes the
        // server sent to it (= the device's DOWNLOAD). Every PER-DEVICE stat
        // below (lifetime totals, history samples, live rates, session
        // snapshots, StatsDelta) must present the DEVICE's own perspective —
        // "RX" on a device card means the device's download — so we re-base
        // here. The server rollup (`srv_totals` above) and the keepalive
        // liveness check deliberately keep the raw server-perspective
        // `drx`/`dtx`/`rx_total`/`tx_total`, since a server card correctly
        // shows the server's own interface direction.
        let dev_rx_delta = dtx; // device download this tick (server → peer)
        let dev_tx_delta = drx; // device upload this tick   (peer → server)
        let dev_rx_total = tx_total; // absolute device-download counter
        let dev_tx_total = rx_total; // absolute device-upload counter

        match (prev, online) {
            // Fresh connection (None → online is "this peer was already
            // online when we booted, sweep-then-open kicks off a fresh
            // session"; Some(false) → online is the regular reconnect).
            (None, true) | (Some(false), true) => {
                if let Err(e) = connection_sessions::open(
                    pool,
                    device_id,
                    user_id,
                    endpoint_now.as_deref(),
                    dev_rx_total as i64,
                    dev_tx_total as i64,
                    now,
                )
                .await
                {
                    warn!(?e, %device_id, "connection_sessions open failed");
                }
            }
            // Disconnect — close the open row, stamping end-state
            // endpoint + cumulative byte counters.
            (Some(true), false) => {
                if let Err(e) = connection_sessions::close(
                    pool,
                    device_id,
                    endpoint_now.as_deref(),
                    dev_rx_total as i64,
                    dev_tx_total as i64,
                    now,
                )
                .await
                {
                    warn!(?e, %device_id, "connection_sessions close failed");
                }
            }
            _ => {}
        }
        prev_online.insert(device_id, online);

        // Peers that have never completed a handshake can still appear in
        // `wg show dump` with non-zero rx/tx counters — these are bytes
        // accepted for the initiator handshake itself, not tunneled
        // traffic. Reporting them as live "rates" makes a brand-new
        // device's card show changing numbers before it has ever
        // connected, which is misleading. Suppress the rate fields when
        // there's no real handshake on record so the live stream only
        // carries traffic for sessions that actually established.
        let report_rates = latest_handshake > 0;

        // Persist the delta for historical aggregation. Without this row
        // the hourly/daily rollups have nothing to sum, so the dashboard
        // chart stays empty in real WG mode. Best-effort — a transient DB
        // error doesn't stop the live broadcast below. Skipped for peers
        // that haven't handshook yet so initiator-handshake bytes don't
        // pollute the history.
        if report_rates && (drx > 0 || dtx > 0)
            && let Err(e) = bandwidth::insert_sample(
                pool,
                device_id,
                now,
                dev_rx_delta as i64,
                dev_tx_delta as i64,
            )
            .await
            {
                warn!(?e, %device_id, "bandwidth sample insert failed");
            }

        // Maintain the device's authoritative lifetime totals (the "Total
        // RX/TX" the UI shows). On first sight reconcile against the live WG
        // counter (GREATEST — catch up downtime, keep the larger lifetime
        // across a counter reset); on later ticks add this tick's delta. We
        // mirror the result in `lifetimes` so idle ticks below can still
        // report an accurate absolute total without a DB round-trip.
        if report_rates {
            if first_sight {
                match devices::seed_lifetime(
                    pool,
                    device_id,
                    dev_rx_total as i64,
                    dev_tx_total as i64,
                )
                .await
                {
                    Ok((lr, lt)) => {
                        lifetimes.insert(device_id, (lr.max(0) as u64, lt.max(0) as u64));
                    }
                    Err(e) => warn!(?e, %device_id, "seed_lifetime failed"),
                }
            } else if drx > 0 || dtx > 0 {
                match devices::accumulate_lifetime(
                    pool,
                    device_id,
                    dev_rx_delta as i64,
                    dev_tx_delta as i64,
                )
                .await
                {
                    Ok((lr, lt)) => {
                        lifetimes.insert(device_id, (lr.max(0) as u64, lt.max(0) as u64));
                    }
                    Err(e) => warn!(?e, %device_id, "accumulate_lifetime failed"),
                }
            }
        }
        let (total_rx_bytes, total_tx_bytes) =
            lifetimes.get(&device_id).copied().unwrap_or((0, 0));

        // Per-user monthly quota: fold this device's traffic into the owner's
        // monthly counter and notify once on crossing 90% (warning) then 100%
        // (limit). `quota_tier` tracks the highest tier already announced so we
        // don't repeat, and re-arms when the month resets (usage drops back).
        if report_rates && (drx > 0 || dtx > 0) {
            // Per-device monthly counter — feeds the per-device quota that the
            // API's enforcement sweep reads. Fire-and-forget: the worker only
            // measures; the API (which owns the WG controller) pauses/resumes.
            if let Err(e) =
                devices::add_monthly_usage(pool, device_id, (drx + dtx) as i64).await
            {
                warn!(?e, %device_id, "device monthly usage update failed");
            }
            match users::add_monthly_usage(pool, user_id, (drx + dtx) as i64).await {
                Ok((current, Some(cap))) if cap > 0 => {
                    let tier: u8 = if current >= cap {
                        2
                    } else if current.saturating_mul(10) >= cap.saturating_mul(9) {
                        1
                    } else {
                        0
                    };
                    let prev_tier = quota_tier.get(&user_id).copied().unwrap_or(0);
                    if tier > prev_tier {
                        let (title, body, level) = if tier >= 2 {
                            (
                                "Monthly data limit reached",
                                "You've used your full monthly data allowance.",
                                NotifyLevel::Error,
                            )
                        } else {
                            (
                                "Approaching your data limit",
                                "You've used over 90% of this month's data.",
                                NotifyLevel::Warning,
                            )
                        };
                        let _ = tx
                            .send((
                                format!("events.user.{user_id}"),
                                Event::Notify {
                                    user_id: Some(user_id),
                                    level,
                                    title: title.to_string(),
                                    body: Some(body.to_string()),
                                    url: Some("/app".to_string()),
                                    tag: Some(format!("quota-{user_id}")),
                                },
                            ))
                            .await;
                    }
                    if tier != prev_tier {
                        quota_tier.insert(user_id, tier);
                    }
                }
                Ok(_) => {} // no cap configured → unlimited, nothing to warn
                Err(e) => warn!(?e, %user_id, "monthly usage update failed"),
            }
        }

        let rate_rx_bps = if report_rates { dev_rx_delta / secs * 8 } else { 0 };
        let rate_tx_bps = if report_rates { dev_tx_delta / secs * 8 } else { 0 };

        // Fold this peer's rate into its in-progress 1-minute candle. Every
        // tick (including idle 0-rate ones) counts toward the sample so the
        // minute's average is faithful; all-idle minutes are dropped at flush.
        candle_acc
            .devices
            .entry(device_id)
            .or_default()
            .observe(rate_rx_bps as i64, rate_tx_bps as i64);

        let event = Event::StatsDelta {
            device_id,
            user_id,
            rx_bytes: if report_rates { dev_rx_delta } else { 0 },
            tx_bytes: if report_rates { dev_tx_delta } else { 0 },
            rate_rx_bps,
            rate_tx_bps,
            total_rx_bytes,
            total_tx_bytes,
            ts_ms: now_ms,
        };
        let topic = format!("stats.peer.{}", device_id);
        if tx.send((topic, event)).await.is_err() {
            return Ok(peers_seen);
        }
        peers_seen += 1;
    }

    // Emit per-server tick. Also persists to server_samples. If wg show
    // returned no peers for a server we still want a zero-tick row so
    // the chart doesn't get a gap on idle stretches — list all active
    // servers and zero-fill the ones not in srv_totals.
    let all_servers = servers::list_active(pool).await.unwrap_or_default();
    for s in all_servers {
        let (srv_rx, srv_tx, peers, online, handshakes) =
            srv_totals.get(&s.id).copied().unwrap_or((0, 0, 0, 0, 0));
        let srv_rate_rx = (srv_rx / secs * 8) as i64;
        let srv_rate_tx = (srv_tx / secs * 8) as i64;
        // Server-aggregate candle: fold the summed peer rate for this minute.
        candle_acc
            .servers
            .entry(s.id)
            .or_default()
            .observe(srv_rate_rx, srv_rate_tx);
        if let Err(e) = server_samples::insert(
            pool,
            &server_samples::ServerSample {
                server_id: s.id,
                sampled_at: now,
                total_rx_bytes: srv_rx as i64,
                total_tx_bytes: srv_tx as i64,
                peer_count: peers as i32,
                online_count: online as i32,
                handshake_count: handshakes as i32,
            },
        )
        .await
        {
            warn!(server = %s.id, ?e, "server_sample insert failed");
        }
        let _ = tx
            .send((
                format!("stats.server.{}", s.id),
                Event::ServerSample {
                    server_id: s.id,
                    total_rx_bytes: srv_rx,
                    total_tx_bytes: srv_tx,
                    rate_rx_bps: srv_rx / secs * 8,
                    rate_tx_bps: srv_tx / secs * 8,
                    peer_count: peers,
                    online_count: online,
                    handshake_count: handshakes,
                    ts_ms: now_ms,
                },
            ))
            .await;
    }

    Ok(peers_seen)
}
