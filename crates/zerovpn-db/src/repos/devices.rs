use std::net::IpAddr;

use ipnetwork::IpNetwork;
use uuid::Uuid;
use zerovpn_core::models::{Device, DeviceOs, DeviceStatus, DeviceType};

use crate::PgPool;

pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> sqlx::Result<Vec<Device>> {
    // `display_order` is set by the drag-reorder UI; older rows are NULL
    // and fall back to created_at-desc so the list still reads sensibly
    // before a user has touched the order.
    sqlx::query_as::<_, Device>(
        r#"SELECT id, user_id, server_id, name, os, device_type, public_key, allocated_ip, status,
                  allowed_ips_override,
                  last_handshake_at, created_at, private_key_encrypted
           FROM devices
           WHERE user_id = $1 AND status <> 'revoked'
           ORDER BY display_order NULLS LAST, created_at DESC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Bulk-assign `display_order` for a list of device ids. Index in the
/// input slice becomes the new position. Devices not in the list are
/// untouched. Scoped to `user_id` so one user can't reorder another's.
/// Runs in a single statement using `unnest` so it's atomic.
pub async fn set_display_order(
    pool: &PgPool,
    user_id: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let positions: Vec<i32> = (0..ids.len() as i32).collect();
    let res = sqlx::query(
        r#"UPDATE devices AS d
              SET display_order = v.pos
             FROM (
                 SELECT UNNEST($2::uuid[]) AS id, UNNEST($3::int[]) AS pos
             ) AS v
            WHERE d.user_id = $1
              AND d.id = v.id"#,
    )
    .bind(user_id)
    .bind(ids)
    .bind(&positions)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn find_for_user(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Uuid,
) -> sqlx::Result<Option<Device>> {
    sqlx::query_as::<_, Device>(
        r#"SELECT id, user_id, server_id, name, os, device_type, public_key, allocated_ip, status,
                  allowed_ips_override,
                  last_handshake_at, created_at, private_key_encrypted
           FROM devices
           WHERE user_id = $1 AND id = $2"#,
    )
    .bind(user_id)
    .bind(device_id)
    .fetch_optional(pool)
    .await
}

/// Latest observed WG peer endpoint (`host:port`) + when it was first
/// seen, per non-revoked device for a user. Returned separately from the
/// `Device` row — these columns are deliberately kept off the core
/// `Device` model (mirroring the admin detail views), so the common
/// SELECTs stay lean and the user-facing list/get handlers merge this in.
pub async fn peer_endpoints_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> sqlx::Result<Vec<(Uuid, Option<String>, Option<time::OffsetDateTime>)>> {
    sqlx::query_as(
        r#"SELECT id, last_peer_endpoint, last_peer_endpoint_at
             FROM devices
            WHERE user_id = $1 AND status <> 'revoked'"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Per-device monthly quota snapshot for a user's non-revoked devices:
/// `(device_id, monthly_byte_cap, current_month_bytes, auto_paused)`. Kept off
/// the core `Device` model (like endpoints / totals) and merged into the
/// list/get responses so the device card can render a quota bar.
pub async fn quota_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> sqlx::Result<Vec<(Uuid, Option<i64>, i64, bool)>> {
    sqlx::query_as(
        r#"SELECT id, monthly_byte_cap, current_month_bytes, auto_paused
             FROM devices
            WHERE user_id = $1 AND status <> 'revoked'"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Admin-only: every non-revoked device across all users. Powers the
/// admin topology view, so devices come back in a stable order
/// (`user_id`, then `created_at`) — the renderer groups by user, and
/// stable ordering keeps the graph from re-laying-out on each refetch.
pub async fn list_all_active(pool: &PgPool) -> sqlx::Result<Vec<Device>> {
    sqlx::query_as::<_, Device>(
        r#"SELECT id, user_id, server_id, name, os, device_type, public_key, allocated_ip, status,
                  allowed_ips_override,
                  last_handshake_at, created_at, private_key_encrypted
           FROM devices
           WHERE status <> 'revoked'
           ORDER BY user_id, created_at"#,
    )
    .fetch_all(pool)
    .await
}

pub async fn count_active_for_server(pool: &PgPool, server_id: Uuid) -> sqlx::Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM devices WHERE server_id = $1 AND status = 'active'",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub struct NewDevice<'a> {
    pub user_id: Uuid,
    pub server_id: Uuid,
    pub name: &'a str,
    pub os: DeviceOs,
    pub device_type: DeviceType,
    pub public_key: &'a str,
    pub preshared_key_encrypted: Option<&'a [u8]>,
    pub allocated_ip: IpNetwork,
    /// KEK-encrypted WG private key. None for default zero-knowledge
    /// devices; Some(...) when the user opted in at create time.
    pub private_key_encrypted: Option<&'a [u8]>,
}

pub async fn create(pool: &PgPool, d: NewDevice<'_>) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO devices (id, user_id, server_id, name, os, device_type, public_key,
                                preshared_key_encrypted, allocated_ip,
                                private_key_encrypted)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    )
    .bind(id)
    .bind(d.user_id)
    .bind(d.server_id)
    .bind(d.name)
    .bind(d.os)
    .bind(d.device_type)
    .bind(d.public_key)
    .bind(d.preshared_key_encrypted)
    .bind(d.allocated_ip)
    .bind(d.private_key_encrypted)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Replace the stored encrypted private key on an existing device. Used by
/// rotate-keys when the device was created with `store_private_key`. Pass
/// `Some(&[])` to set NULL would be ambiguous, so we require an Option:
/// `None` to clear (the device opts back out of storage), `Some(...)` to
/// store fresh ciphertext.
pub async fn set_private_key_encrypted(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Uuid,
    encrypted: Option<&[u8]>,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devices SET private_key_encrypted = $3 WHERE user_id = $1 AND id = $2",
    )
    .bind(user_id)
    .bind(device_id)
    .bind(encrypted)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn set_status(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Uuid,
    status: DeviceStatus,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devices SET status = $3 WHERE user_id = $1 AND id = $2",
    )
    .bind(user_id)
    .bind(device_id)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Swap the device's public key after a per-device key rotation. The
/// caller is expected to have already generated a fresh private+public
/// keypair (private key never touches this row — it ships once in the
/// rendered config and is discarded). Scoped to (user_id, device_id) so
/// one user can't rotate another's peer.
pub async fn update_public_key(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Uuid,
    public_key: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devices SET public_key = $3 WHERE user_id = $1 AND id = $2",
    )
    .bind(user_id)
    .bind(device_id)
    .bind(public_key)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// `public_key → (device_id, user_id, server_id)` lookup so the WG poller
/// can attribute each `wg show dump` line and roll per-tick stats up to
/// the owning server.
pub async fn pubkey_index(
    pool: &PgPool,
) -> sqlx::Result<std::collections::HashMap<String, (Uuid, Uuid, Uuid)>> {
    let rows: Vec<(String, Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT public_key, id, user_id, server_id
           FROM devices WHERE status <> 'revoked'",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(pk, id, uid, sid)| (pk, (id, uid, sid)))
        .collect())
}

/// Persist the most recently observed WG peer endpoint for a device.
/// The caller (`wg_poller`) only calls this when the endpoint changed
/// against the in-memory baseline, so the row touch is rare. Endpoint
/// is stored as TEXT (carries `host:port`); see migration 15.
pub async fn set_last_peer_endpoint(
    pool: &PgPool,
    device_id: Uuid,
    endpoint: &str,
    observed_at: time::OffsetDateTime,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devices
            SET last_peer_endpoint    = $2,
                last_peer_endpoint_at = $3
          WHERE id = $1",
    )
    .bind(device_id)
    .bind(endpoint)
    .bind(observed_at)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Update last_handshake_at without churning rows when the value didn't change.
pub async fn touch_handshake(
    pool: &PgPool,
    device_id: Uuid,
    handshake_at: time::OffsetDateTime,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devices SET last_handshake_at = $2
          WHERE id = $1 AND (last_handshake_at IS NULL OR last_handshake_at < $2)",
    )
    .bind(device_id)
    .bind(handshake_at)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Seed a device's lifetime counters to at least the live WireGuard counter,
/// returning the resulting totals. Called by the poller on the **first sight**
/// of a peer each worker session: `GREATEST` means a continuously-connected
/// peer catches up bytes transferred while the worker was down (matching
/// `wg show`), while a peer whose counter reset to a smaller value (re-add /
/// reboot) keeps its larger accumulated lifetime. Never decreases.
pub async fn seed_lifetime(
    pool: &PgPool,
    device_id: Uuid,
    counter_rx: i64,
    counter_tx: i64,
) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        r#"UPDATE devices
              SET lifetime_rx_bytes = GREATEST(lifetime_rx_bytes, $2),
                  lifetime_tx_bytes = GREATEST(lifetime_tx_bytes, $3)
            WHERE id = $1
        RETURNING lifetime_rx_bytes, lifetime_tx_bytes"#,
    )
    .bind(device_id)
    .bind(counter_rx)
    .bind(counter_tx)
    .fetch_one(pool)
    .await
}

/// Add this tick's RX/TX delta to a device's lifetime counters, returning the
/// new absolute totals (which the worker forwards in `StatsDelta` so clients
/// show an exact, live-growing total). The increment is atomic, so only the
/// worker ever writes these and concurrency is a non-issue.
pub async fn accumulate_lifetime(
    pool: &PgPool,
    device_id: Uuid,
    add_rx: i64,
    add_tx: i64,
) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        r#"UPDATE devices
              SET lifetime_rx_bytes = lifetime_rx_bytes + $2,
                  lifetime_tx_bytes = lifetime_tx_bytes + $3
            WHERE id = $1
        RETURNING lifetime_rx_bytes, lifetime_tx_bytes"#,
    )
    .bind(device_id)
    .bind(add_rx)
    .bind(add_tx)
    .fetch_one(pool)
    .await
}

/// Sum of every non-revoked device's lifetime RX+TX for a user — the
/// authoritative all-time usage (the device cards' "Total"). The dashboard
/// derives accurate monthly usage as a delta off this trustworthy figure
/// (see the `/me/usage` handler), sidestepping the drift-prone monthly
/// accumulators.
pub async fn user_lifetime_total(pool: &PgPool, user_id: Uuid) -> sqlx::Result<i64> {
    let row: (i64,) = sqlx::query_as(
        r#"SELECT COALESCE(SUM(lifetime_rx_bytes + lifetime_tx_bytes), 0)::BIGINT
             FROM devices
            WHERE user_id = $1 AND status <> 'revoked'"#,
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Per-server cumulative lifetime RX/TX, keyed by `server_id` — the sum of
/// each server's (non-revoked) devices' lifetime counters. Powers the admin
/// server-live card's RX/TX totals. Same trustworthy source as the fleet /
/// device totals (not the drift-prone aggregates).
pub async fn server_lifetime_totals(
    pool: &PgPool,
) -> sqlx::Result<Vec<(Uuid, i64, i64)>> {
    sqlx::query_as(
        r#"SELECT server_id,
                  COALESCE(SUM(lifetime_rx_bytes), 0)::BIGINT,
                  COALESCE(SUM(lifetime_tx_bytes), 0)::BIGINT
             FROM devices
            WHERE status <> 'revoked'
            GROUP BY server_id"#,
    )
    .fetch_all(pool)
    .await
}

/// Fold this tick's RX+TX delta into the device's monthly quota counter,
/// resetting the counter (and advancing `quota_resets_at`) when the month has
/// rolled over — mirrors [`crate::repos::users::add_monthly_usage`] but scoped
/// to one device. Returns the new monthly total + the device's own cap so the
/// caller can decide whether the per-device limit was crossed.
pub async fn add_monthly_usage(
    pool: &PgPool,
    device_id: Uuid,
    delta_bytes: i64,
) -> sqlx::Result<(i64, Option<i64>)> {
    let now = time::OffsetDateTime::now_utc();
    let next_reset = crate::repos::users::first_of_next_month(now);
    let row: Option<(i64, Option<i64>)> = sqlx::query_as(
        r#"UPDATE devices
              SET current_month_bytes = CASE
                    WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                      THEN $3
                    ELSE current_month_bytes + $3
                  END,
                  quota_resets_at = CASE
                    WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                      THEN $4
                    ELSE quota_resets_at
                  END
            WHERE id = $1
        RETURNING current_month_bytes, monthly_byte_cap"#,
    )
    .bind(device_id)
    .bind(now)
    .bind(delta_bytes)
    .bind(next_reset)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or((0, None)))
}

/// Admin: set (or clear, with `None`) a device's monthly byte cap.
pub async fn set_quota(
    pool: &PgPool,
    device_id: Uuid,
    cap: Option<i64>,
) -> sqlx::Result<u64> {
    let res = sqlx::query("UPDATE devices SET monthly_byte_cap = $2 WHERE id = $1")
        .bind(device_id)
        .bind(cap)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// All allocated IPs for a server, used to seed the in-memory bitmap on boot.
pub async fn allocated_ips_for_server(pool: &PgPool, server_id: Uuid) -> sqlx::Result<Vec<IpAddr>> {
    let rows: Vec<(IpNetwork,)> = sqlx::query_as(
        "SELECT allocated_ip FROM devices WHERE server_id = $1 AND status <> 'revoked'",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(n,)| n.ip()).collect())
}

// FromRow on Device is derived in zerovpn-core::models.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Device>> {
    sqlx::query_as("SELECT * FROM devices WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}
