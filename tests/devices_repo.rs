//! Device + per-device-quota repo tests against a real Postgres in a
//! testcontainers-managed container. Unlike `users_repo.rs` (which hand-
//! applies the first couple of migrations), these run the *full* migration
//! set via `zerovpn_db::run_migrations` so the device quota columns added by
//! later migrations are present.
//!
//! Requires Docker. Image pinned to `postgres:18-alpine` to match the
//! project's Postgres 18 target.

use ipnetwork::IpNetwork;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use zerovpn_core::models::{DeviceOs, DeviceStatus, DeviceType, UserRole, UserStatus};
use zerovpn_db::repos::{devices, servers, users};

/// Boot a throwaway Postgres, connect a pool, and apply every migration.
/// Returns the container guard (kept alive for the test) and the pool.
async fn setup() -> anyhow::Result<(testcontainers::ContainerAsync<Postgres>, sqlx::PgPool)> {
    let pg = Postgres::default()
        .with_db_name("zerovpn")
        .with_user("zerovpn")
        .with_password("zerovpn")
        .with_tag("18-alpine")
        .start()
        .await?;
    let port = pg.get_host_port_ipv4(5432).await?;
    let url = format!("postgres://zerovpn:zerovpn@127.0.0.1:{port}/zerovpn?sslmode=disable");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    zerovpn_db::run_migrations(&pool).await?;
    Ok((pg, pool))
}

async fn seed_user(pool: &sqlx::PgPool, email: &str) -> anyhow::Result<Uuid> {
    // Placeholder hash — the DB doesn't validate its format and these tests
    // don't authenticate.
    Ok(users::create(pool, email, "!placeholder-hash", UserRole::User, UserStatus::Active).await?)
}

async fn seed_server(pool: &sqlx::PgPool) -> anyhow::Result<Uuid> {
    let cidr: IpNetwork = "10.10.0.0/22".parse()?;
    Ok(servers::create(
        pool,
        servers::NewServer {
            name: "test-srv",
            region: "test",
            endpoint_host: "vpn.test",
            endpoint_port: 51820,
            public_key: "srv-public-key",
            private_key_encrypted: b"test-encrypted-key",
            cidr,
            mtu: 1420,
        },
    )
    .await?)
}

async fn seed_device(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    server_id: Uuid,
    ip: &str,
) -> anyhow::Result<Uuid> {
    let allocated_ip: IpNetwork = ip.parse()?;
    Ok(devices::create(
        pool,
        devices::NewDevice {
            user_id,
            server_id,
            name: "laptop",
            os: DeviceOs::Other,
            device_type: DeviceType::Other,
            public_key: "device-public-key",
            preshared_key_encrypted: None,
            allocated_ip,
            private_key_encrypted: None,
        },
    )
    .await?)
}

#[tokio::test]
async fn device_crud_and_pause_unpause() -> anyhow::Result<()> {
    let (_pg, pool) = setup().await?;
    let user = seed_user(&pool, "bob@example.com").await?;
    let server = seed_server(&pool).await?;
    let device = seed_device(&pool, user, server, "10.10.0.5/32").await?;

    // Create → visible, Active, IP reserved.
    let list = devices::list_for_user(&pool, user).await?;
    assert_eq!(list.len(), 1, "the new device shows in the user's list");
    let got = devices::find_for_user(&pool, user, device).await?.expect("found");
    assert_eq!(got.status, DeviceStatus::Active);
    assert_eq!(
        devices::allocated_ips_for_server(&pool, server).await?.len(),
        1,
        "active device's IP is reserved"
    );

    // Pause → status flips; a paused device is still listed (only revoked is
    // filtered out of list_for_user).
    devices::set_status(&pool, user, device, DeviceStatus::Paused).await?;
    let paused = devices::find_for_user(&pool, user, device).await?.expect("found");
    assert_eq!(paused.status, DeviceStatus::Paused);
    assert_eq!(
        devices::list_for_user(&pool, user).await?.len(),
        1,
        "paused devices stay in the list"
    );

    // Unpause → back to Active.
    devices::set_status(&pool, user, device, DeviceStatus::Active).await?;
    let active = devices::find_for_user(&pool, user, device).await?.expect("found");
    assert_eq!(active.status, DeviceStatus::Active);

    // Revoke → dropped from list_for_user and its IP freed for reuse, though
    // find_for_user (id-scoped) still returns the row for history.
    devices::set_status(&pool, user, device, DeviceStatus::Revoked).await?;
    assert!(
        devices::list_for_user(&pool, user).await?.is_empty(),
        "revoked devices are filtered from the list"
    );
    assert!(
        devices::allocated_ips_for_server(&pool, server).await?.is_empty(),
        "revoked device's IP is freed"
    );
    let revoked = devices::find_for_user(&pool, user, device).await?.expect("row kept");
    assert_eq!(revoked.status, DeviceStatus::Revoked);
    Ok(())
}

#[tokio::test]
async fn device_monthly_cap_persists_and_resets() -> anyhow::Result<()> {
    let (_pg, pool) = setup().await?;
    let user = seed_user(&pool, "carol@example.com").await?;
    let server = seed_server(&pool).await?;
    let device = seed_device(&pool, user, server, "10.10.0.6/32").await?;

    // No cap initially.
    let rows = devices::quota_for_user(&pool, user).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], (device, None, 0, false), "fresh device: no cap, no usage");

    // Set a per-device cap and accumulate usage within the cycle.
    let cap = 10_000_i64;
    devices::set_quota(&pool, device, Some(cap)).await?;
    let (used1, got_cap) = devices::add_monthly_usage(&pool, device, 4_000).await?;
    assert_eq!((used1, got_cap), (4_000, Some(cap)));
    let (used2, _) = devices::add_monthly_usage(&pool, device, 3_000).await?;
    assert_eq!(used2, 7_000, "usage accumulates within the cycle");

    // Force the reset boundary into the past; the next add starts a fresh
    // cycle rather than accumulating.
    sqlx::query("UPDATE devices SET quota_resets_at = $2 WHERE id = $1")
        .bind(device)
        .bind(OffsetDateTime::now_utc() - Duration::days(40))
        .execute(&pool)
        .await?;
    let (used3, _) = devices::add_monthly_usage(&pool, device, 250).await?;
    assert_eq!(used3, 250, "a new cycle resets the counter to the delta");

    // Clearing the cap persists as NULL.
    devices::set_quota(&pool, device, None).await?;
    let cleared = devices::quota_for_user(&pool, user).await?;
    assert_eq!(cleared[0].1, None, "cap cleared back to NULL");
    Ok(())
}
