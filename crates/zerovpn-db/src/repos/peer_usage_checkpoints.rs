//! Durable accounting for resettable WireGuard peer counters.
//!
//! A raw counter checkpoint, device lifetime total, monthly quota, and
//! bandwidth sample advance in one transaction. A worker restart can then
//! resume from the committed raw value without replaying old bytes.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::PgPool;

#[derive(Debug)]
pub struct AccountedUsage {
    /// Raw server perspective: RX is peer upload, TX is peer download.
    pub server_rx_delta: i64,
    pub server_tx_delta: i64,
    /// Time since the last durable observation, for honest live rates after
    /// a worker outage. At least one second.
    pub elapsed_seconds: u64,
    pub device_lifetime_rx: i64,
    pub device_lifetime_tx: i64,
    pub user_quota: Option<(i64, Option<i64>)>,
}

/// Account one observation. The first observation installs a baseline and
/// preserves existing lifetime totals; historical raw counters cannot be
/// inferred from those totals. A new source generation or a raw regression
/// starts a new counter epoch. Until the host agent supplies an explicit
/// generation, the worker passes a stable legacy source name.
#[allow(clippy::too_many_arguments)]
pub async fn account(
    pool: &PgPool,
    device_id: Uuid,
    user_id: Uuid,
    server_id: Uuid,
    public_key: &str,
    source_generation: &str,
    server_rx: i64,
    server_tx: i64,
    sampled_at: OffsetDateTime,
    has_handshake: bool,
) -> sqlx::Result<AccountedUsage> {
    if server_rx < 0 || server_tx < 0 || source_generation.is_empty() {
        return Err(sqlx::Error::Protocol(
            "invalid WireGuard counter observation".into(),
        ));
    }

    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO peer_usage_checkpoints
           (device_id, public_key, source_generation, raw_server_rx_bytes,
            raw_server_tx_bytes, sampled_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (device_id) DO NOTHING",
    )
    .bind(device_id)
    .bind(public_key)
    .bind(source_generation)
    .bind(server_rx)
    .bind(server_tx)
    .bind(sampled_at)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;

    let (old_key, old_source, epoch, old_rx, old_tx, old_at): (
        String,
        String,
        i64,
        i64,
        i64,
        OffsetDateTime,
    ) = sqlx::query_as(
        "SELECT public_key, source_generation, counter_epoch,
                raw_server_rx_bytes, raw_server_tx_bytes, sampled_at
           FROM peer_usage_checkpoints WHERE device_id = $1 FOR UPDATE",
    )
    .bind(device_id)
    .fetch_one(&mut *tx)
    .await?;

    let changed_source = old_key != public_key || old_source != source_generation;
    let regressed = server_rx < old_rx || server_tx < old_tx;
    let elapsed_seconds = (sampled_at - old_at).whole_seconds().max(1) as u64;
    let (server_rx_delta, server_tx_delta) = if inserted || sampled_at <= old_at {
        (0, 0)
    } else if changed_source || regressed {
        // This is safe when the generation is known to have started at zero.
        // The legacy path detects a reset only when a counter goes backwards.
        (server_rx, server_tx)
    } else {
        (server_rx - old_rx, server_tx - old_tx)
    };
    let (server_rx_delta, server_tx_delta) = if has_handshake {
        (server_rx_delta, server_tx_delta)
    } else {
        (0, 0)
    };

    if !inserted && sampled_at > old_at {
        sqlx::query(
            "UPDATE peer_usage_checkpoints
                SET public_key = $2, source_generation = $3,
                    counter_epoch = $4, raw_server_rx_bytes = $5,
                    raw_server_tx_bytes = $6, sampled_at = $7
              WHERE device_id = $1",
        )
        .bind(device_id)
        .bind(public_key)
        .bind(source_generation)
        .bind(if changed_source || regressed {
            epoch + 1
        } else {
            epoch
        })
        .bind(server_rx)
        .bind(server_tx)
        .bind(sampled_at)
        .execute(&mut *tx)
        .await?;
    }

    let device_rx = server_tx_delta;
    let device_tx = server_rx_delta;
    let combined = device_rx
        .checked_add(device_tx)
        .ok_or_else(|| sqlx::Error::Protocol("WireGuard counter delta overflow".into()))?;
    let next_reset = super::users::first_of_next_month(sampled_at);

    let (device_lifetime_rx, device_lifetime_tx): (i64, i64) = sqlx::query_as(
        "UPDATE devices
            SET lifetime_rx_bytes = lifetime_rx_bytes + $2,
                lifetime_tx_bytes = lifetime_tx_bytes + $3,
                current_month_bytes = CASE
                    WHEN $4 > 0 AND (quota_resets_at IS NULL OR quota_resets_at < $5)
                      THEN $4
                    WHEN $4 > 0 THEN current_month_bytes + $4
                    ELSE current_month_bytes END,
                quota_resets_at = CASE
                    WHEN $4 > 0 AND (quota_resets_at IS NULL OR quota_resets_at < $5)
                      THEN $6 ELSE quota_resets_at END
          WHERE id = $1
      RETURNING lifetime_rx_bytes, lifetime_tx_bytes",
    )
    .bind(device_id)
    .bind(device_rx)
    .bind(device_tx)
    .bind(combined)
    .bind(sampled_at)
    .bind(next_reset)
    .fetch_one(&mut *tx)
    .await?;

    let user_quota = if combined > 0 {
        sqlx::query(
            "INSERT INTO server_vpn_usage
                 (server_id, total_server_rx_bytes, total_server_tx_bytes)
               VALUES ($1, $2, $3)
             ON CONFLICT (server_id) DO UPDATE
                SET total_server_rx_bytes = server_vpn_usage.total_server_rx_bytes
                                                + EXCLUDED.total_server_rx_bytes,
                    total_server_tx_bytes = server_vpn_usage.total_server_tx_bytes
                                                + EXCLUDED.total_server_tx_bytes",
        )
        .bind(server_id)
        .bind(server_rx_delta)
        .bind(server_tx_delta)
        .execute(&mut *tx)
        .await?;

        let value: (i64, Option<i64>) = sqlx::query_as(
            "UPDATE users
                SET current_month_bytes = CASE
                      WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                        THEN $3 ELSE current_month_bytes + $3 END,
                    quota_resets_at = CASE
                      WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                        THEN $4 ELSE quota_resets_at END
              WHERE id = $1 AND deleted_at IS NULL
          RETURNING current_month_bytes, monthly_byte_cap",
        )
        .bind(user_id)
        .bind(sampled_at)
        .bind(combined)
        .bind(next_reset)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO bandwidth_samples (device_id, sampled_at, rx_bytes, tx_bytes)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (device_id, sampled_at) DO UPDATE
                SET rx_bytes = bandwidth_samples.rx_bytes + EXCLUDED.rx_bytes,
                    tx_bytes = bandwidth_samples.tx_bytes + EXCLUDED.tx_bytes",
        )
        .bind(device_id)
        .bind(sampled_at)
        .bind(device_rx)
        .bind(device_tx)
        .execute(&mut *tx)
        .await?;
        Some(value)
    } else {
        None
    };

    tx.commit().await?;
    Ok(AccountedUsage {
        server_rx_delta,
        server_tx_delta,
        elapsed_seconds,
        device_lifetime_rx,
        device_lifetime_tx,
        user_quota,
    })
}

/// Durable server-perspective VPN peer totals. These are payload/accounting
/// counters, not Docker network counters or raw wg0 interface packet bytes.
pub async fn server_totals(pool: &PgPool, server_id: Uuid) -> sqlx::Result<(i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT total_server_rx_bytes, total_server_tx_bytes
           FROM server_vpn_usage WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or((0, 0)))
}
