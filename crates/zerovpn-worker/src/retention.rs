//! Retention enforcement.
//!
//! Runs every 6 hours (and once on startup for fast dev feedback).
//!
//! **Account-lifecycle cleanup** — rows that are pure overhead once their
//! TTL has passed:
//! - Verification tokens older than 24 h (consumed or expired) — the email
//!   link is dead, the row is just bytes.
//! - Soft-deleted users older than 30 d — cascade-removes their devices,
//!   sessions, etc., reclaiming storage.
//! - Pending-verification accounts older than 7 d — the verify-email link
//!   expired at 24 h and the row only blocks the email from re-registering.
//!
//! **Operational-data TTLs** — high-volume tables are purged past a fixed
//! window so they don't grow without bound:
//! - `bandwidth_samples` (raw 1 s ticks)  — 30 d
//! - `server_samples`    (host metrics)   — 30 d
//! - `destination_ips`   (flow ingest)    — 30 d
//! - `audit_logs`        (audit trail)    — 30 d
//! - `failed_logins`     (auth failures)  — 30 d
//!
//! `bandwidth_aggregates` (hour/day rollups) is intentionally kept: it's
//! the long-term history the dashboards read and is tiny next to the raw
//! samples it's derived from.
//!
//! Operational-data windows default to the constants below but are
//! operator-tunable per table via `ZEROVPN_*_RETENTION_DAYS` env vars (read
//! once at startup — see `RetentionWindows::from_env`). Setting a var to `0`
//! disables that table's purge entirely (keep forever) — the opt-in
//! "unbounded" posture. Account-lifecycle TTLs (verification tokens,
//! soft-deleted users, abandoned signups) are policy, not storage tuning, so
//! they stay fixed.
//!
//! `bandwidth_samples` and `server_samples` are RANGE-partitioned on
//! `sampled_at`; a plain DELETE is correct but leaves empty partitions
//! behind — dropping whole expired partitions would be cheaper and is a
//! natural follow-up.

use std::time::Duration;

use time::OffsetDateTime;
use tracing::{info, warn};
use zerovpn_db::{PgPool, repos::candles};

const TICK: Duration = Duration::from_secs(6 * 3600);

const SOFT_DELETE_PURGE_DAYS: i64 = 30;
/// Pending-verification accounts older than this are dropped outright.
/// Their verify-email TTL is 24 h, so by day 7 the link is long-dead and
/// the row is just blocking the email from being re-registered.
const PENDING_VERIFICATION_PURGE_DAYS: i64 = 7;

const SAMPLE_RETENTION_DAYS: i64 = 30;
const SERVER_SAMPLE_RETENTION_DAYS: i64 = 30;
const DEST_RETENTION_DAYS: i64 = 30;
const AUDIT_RETENTION_DAYS: i64 = 30;
const FAILED_LOGIN_RETENTION_DAYS: i64 = 30;
/// 1-minute candles are high-volume (one row per active peer per minute) so
/// they're kept the same 30 d as the raw samples that feed them. The daily
/// candle rollups are tiny (one row per peer per day) and back the long
/// timeframes, so they're kept ~2 years.
const CANDLE_MINUTE_RETENTION_DAYS: i64 = 30;
const CANDLE_DAILY_RETENTION_DAYS: i64 = 730;

/// Operational-data retention windows, in days. `None` means "keep forever"
/// (the operator set the env var to `0`). Read once at startup; the constants
/// above are the fallback defaults when a var is unset or unparseable.
struct RetentionWindows {
    samples: Option<i64>,
    server_samples: Option<i64>,
    destinations: Option<i64>,
    audit: Option<i64>,
    failed_logins: Option<i64>,
    candle_minute: Option<i64>,
    candle_daily: Option<i64>,
}

impl RetentionWindows {
    fn from_env() -> Self {
        let w = Self {
            samples: window_days("ZEROVPN_SAMPLE_RETENTION_DAYS", SAMPLE_RETENTION_DAYS),
            server_samples: window_days(
                "ZEROVPN_SERVER_SAMPLE_RETENTION_DAYS",
                SERVER_SAMPLE_RETENTION_DAYS,
            ),
            destinations: window_days("ZEROVPN_DEST_RETENTION_DAYS", DEST_RETENTION_DAYS),
            audit: window_days("ZEROVPN_AUDIT_RETENTION_DAYS", AUDIT_RETENTION_DAYS),
            failed_logins: window_days(
                "ZEROVPN_FAILED_LOGIN_RETENTION_DAYS",
                FAILED_LOGIN_RETENTION_DAYS,
            ),
            candle_minute: window_days(
                "ZEROVPN_CANDLE_MINUTE_RETENTION_DAYS",
                CANDLE_MINUTE_RETENTION_DAYS,
            ),
            candle_daily: window_days(
                "ZEROVPN_CANDLE_DAILY_RETENTION_DAYS",
                CANDLE_DAILY_RETENTION_DAYS,
            ),
        };
        let fmt = |o: Option<i64>| o.map(|d| format!("{d}d")).unwrap_or_else(|| "∞".into());
        info!(
            samples = %fmt(w.samples),
            server_samples = %fmt(w.server_samples),
            destinations = %fmt(w.destinations),
            audit = %fmt(w.audit),
            failed_logins = %fmt(w.failed_logins),
            candle_minute = %fmt(w.candle_minute),
            candle_daily = %fmt(w.candle_daily),
            "retention windows resolved",
        );
        w
    }
}

/// Parse a retention window (in days) from `key`. Unset or unparseable →
/// `default`. An explicit `0` returns `None` = disable the purge for that
/// table (keep forever).
fn window_days(key: &str, default: i64) -> Option<i64> {
    match std::env::var(key) {
        Ok(v) => match v.trim().parse::<i64>() {
            Ok(0) => None,
            Ok(n) if n > 0 => Some(n),
            _ => {
                warn!(key, value = %v, "invalid retention value; using default");
                Some(default)
            }
        },
        Err(_) => Some(default),
    }
}

pub async fn run(pool: PgPool) {
    info!("retention purger started");
    let windows = RetentionWindows::from_env();
    // Run once on startup for fast feedback in dev.
    if let Err(e) = run_once(&pool, &windows).await {
        warn!(?e, "retention pass failed");
    }
    let mut ticker = tokio::time::interval(TICK);
    ticker.tick().await; // skip immediate
    loop {
        ticker.tick().await;
        if let Err(e) = run_once(&pool, &windows).await {
            warn!(?e, "retention pass failed");
        }
    }
}

async fn run_once(pool: &PgPool, windows: &RetentionWindows) -> sqlx::Result<()> {
    let now = OffsetDateTime::now_utc();

    // --- Account-lifecycle cleanup -------------------------------------

    // Expired OAuth states. `consume` deletes a row on use and filters on
    // `expires_at`, so this is purely a growth bound for abandoned sign-in
    // flows that never hit the callback.
    purge(
        pool,
        "purged expired oauth states",
        "DELETE FROM oauth_states WHERE expires_at <= $1",
        now,
    )
    .await?;

    // Session-metadata rows whose authoritative tower session is gone
    // (expired, logged out, or revoked). Uses the repo helper rather than
    // the `purge()` shape because the predicate is an anti-join, not a
    // timestamp cutoff.
    match zerovpn_db::repos::user_sessions::purge_dangling(pool).await {
        Ok(n) if n > 0 => info!(rows = n, "purged dangling user_sessions rows"),
        Ok(_) => {}
        Err(e) => warn!(?e, "user_sessions purge failed"),
    }

    // Hard-purge users soft-deleted >30 days ago. Cascades remove devices,
    // sessions, etc.
    purge(
        pool,
        "hard-purged soft-deleted users",
        "DELETE FROM users WHERE deleted_at IS NOT NULL AND deleted_at < $1",
        now - time::Duration::days(SOFT_DELETE_PURGE_DAYS),
    )
    .await?;

    // Drop accounts that signed up but never clicked the verify link. Once
    // the verify token has been dead for days the row just blocks the email
    // from being re-registered. Cascades clean up leftover tokens.
    purge(
        pool,
        "purged stale pending-verification accounts",
        "DELETE FROM users
          WHERE status = 'pending_verification'
            AND email_verified_at IS NULL
            AND created_at < $1",
        now - time::Duration::days(PENDING_VERIFICATION_PURGE_DAYS),
    )
    .await?;

    // --- Operational-data TTLs -----------------------------------------

    // Raw bandwidth ticks. Safe to drop past the window because the hourly/
    // daily rollups in `bandwidth_aggregates` already preserve the history
    // the dashboards read.
    if let Some(days) = windows.samples {
        purge(
            pool,
            "purged expired bandwidth samples",
            "DELETE FROM bandwidth_samples WHERE sampled_at < $1",
            now - time::Duration::days(days),
        )
        .await?;
    }

    // Host CPU/mem/net/disk samples.
    if let Some(days) = windows.server_samples {
        purge(
            pool,
            "purged expired server samples",
            "DELETE FROM server_samples WHERE sampled_at < $1",
            now - time::Duration::days(days),
        )
        .await?;
    }

    // Flow-log destination records (keyed on flow start time).
    if let Some(days) = windows.destinations {
        purge(
            pool,
            "purged expired destination IPs",
            "DELETE FROM destination_ips WHERE started_at < $1",
            now - time::Duration::days(days),
        )
        .await?;
    }

    // Audit trail.
    if let Some(days) = windows.audit {
        purge(
            pool,
            "purged expired audit logs",
            "DELETE FROM audit_logs WHERE created_at < $1",
            now - time::Duration::days(days),
        )
        .await?;
    }

    // Failed-login records.
    if let Some(days) = windows.failed_logins {
        purge(
            pool,
            "purged expired failed logins",
            "DELETE FROM failed_logins WHERE attempted_at < $1",
            now - time::Duration::days(days),
        )
        .await?;
    }

    // OHLC candles — 1-minute base rows and daily rollups, each on its own
    // window. The daily table preserves the long-timeframe history once the
    // minute rows it was derived from are gone. A `None` window (env var set
    // to 0) maps to the Unix epoch so nothing is ever older than the cutoff.
    let minute_cut = windows
        .candle_minute
        .map(|d| now - time::Duration::days(d))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let daily_cut = windows
        .candle_daily
        .map(|d| now - time::Duration::days(d))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let removed = candles::prune(pool, minute_cut, daily_cut).await?;
    if removed > 0 {
        info!(rows = removed, "purged expired bandwidth candles");
    }

    Ok(())
}

/// Run a single-parameter DELETE bound to `cutoff` and log how many rows it
/// removed (only when non-zero, to keep idle passes quiet). The SQL may
/// reference `$1` more than once; it is bound a single time.
async fn purge(
    pool: &PgPool,
    label: &'static str,
    sql: &str,
    cutoff: OffsetDateTime,
) -> sqlx::Result<()> {
    let res = sqlx::query(sql).bind(cutoff).execute(pool).await?;
    if res.rows_affected() > 0 {
        info!(rows = res.rows_affected(), "{}", label);
    }
    Ok(())
}
