//! A real Postgres check for restart-safe peer accounting.

use ipnetwork::IpNetwork;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use time::{Duration, OffsetDateTime};
use zerovpn_core::models::{DeviceOs, DeviceType, UserRole, UserStatus};
use zerovpn_db::repos::{devices, peer_usage_checkpoints, servers, users};

#[tokio::test]
async fn existing_totals_survive_restart_and_counter_reset() -> anyhow::Result<()> {
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

    let user = users::create(
        &pool,
        "accounting@example.com",
        "!placeholder-hash",
        UserRole::User,
        UserStatus::Active,
    )
    .await?;
    let server = servers::create(
        &pool,
        servers::NewServer {
            name: "accounting-srv",
            region: "test",
            endpoint_host: "vpn.test",
            endpoint_port: 51820,
            public_key: "test-server-key",
            private_key_encrypted: b"test-encrypted-key",
            cidr: "10.10.0.0/22".parse::<IpNetwork>()?,
            mtu: 1420,
        },
    )
    .await?;
    let device = devices::create(
        &pool,
        devices::NewDevice {
            user_id: user,
            server_id: server,
            name: "laptop",
            os: DeviceOs::Other,
            device_type: DeviceType::Other,
            public_key: "test-peer-key",
            preshared_key_encrypted: None,
            allocated_ip: "10.10.0.5/32".parse::<IpNetwork>()?,
            allowed_ips_override: None,
            private_key_encrypted: None,
        },
    )
    .await?;

    // A migration must not replace totals accumulated before the checkpoint
    // table existed. The first raw observation is only a baseline.
    sqlx::query(
        "UPDATE devices SET lifetime_rx_bytes = 1000, lifetime_tx_bytes = 2000 WHERE id = $1",
    )
    .bind(device)
    .execute(&pool)
    .await?;
    // Simulate the migration's one-time backfill for a device that existed
    // before this test database was migrated.
    sqlx::query(
        "INSERT INTO server_vpn_usage
                    (server_id, total_server_rx_bytes, total_server_tx_bytes)
                 VALUES ($1, 2000, 1000)
                 ON CONFLICT (server_id) DO UPDATE
                   SET total_server_rx_bytes = 2000,
                       total_server_tx_bytes = 1000",
    )
    .bind(server)
    .execute(&pool)
    .await?;
    let now = OffsetDateTime::now_utc();
    let sample = |second: i64| now + Duration::seconds(second);
    let first = peer_usage_checkpoints::account(
        &pool,
        device,
        user,
        server,
        "test-peer-key",
        "legacy:wg0",
        100,
        200,
        sample(0),
        true,
    )
    .await?;
    assert_eq!(
        (first.device_lifetime_rx, first.device_lifetime_tx),
        (1000, 2000)
    );
    assert_eq!((first.server_rx_delta, first.server_tx_delta), (0, 0));

    // A worker restart has no in-memory baseline. The DB checkpoint still
    // identifies exactly the bytes added since the previous observation.
    let second = peer_usage_checkpoints::account(
        &pool,
        device,
        user,
        server,
        "test-peer-key",
        "legacy:wg0",
        130,
        260,
        sample(61),
        true,
    )
    .await?;
    assert_eq!((second.server_rx_delta, second.server_tx_delta), (30, 60));
    assert_eq!(second.elapsed_seconds, 61);
    assert_eq!(
        (second.device_lifetime_rx, second.device_lifetime_tx),
        (1060, 2030)
    );
    let replay = peer_usage_checkpoints::account(
        &pool,
        device,
        user,
        server,
        "test-peer-key",
        "legacy:wg0",
        130,
        260,
        sample(62),
        true,
    )
    .await?;
    assert_eq!((replay.server_rx_delta, replay.server_tx_delta), (0, 0));

    // The new host interface has a distinct generation. Its counters begin
    // at zero, so its first observed bytes add to the old lifetime.
    let host = peer_usage_checkpoints::account(
        &pool,
        device,
        user,
        server,
        "test-peer-key",
        "host:gen-1",
        5,
        7,
        sample(63),
        true,
    )
    .await?;
    assert_eq!(
        (host.device_lifetime_rx, host.device_lifetime_tx),
        (1067, 2035)
    );
    let reset = peer_usage_checkpoints::account(
        &pool,
        device,
        user,
        server,
        "test-peer-key",
        "host:gen-1",
        2,
        3,
        sample(64),
        true,
    )
    .await?;
    assert_eq!(
        (reset.device_lifetime_rx, reset.device_lifetime_tx),
        (1070, 2037)
    );

    let (device_month, user_month): (i64, i64) = sqlx::query_as(
        "SELECT d.current_month_bytes, u.current_month_bytes
           FROM devices d JOIN users u ON u.id = d.user_id WHERE d.id = $1",
    )
    .bind(device)
    .fetch_one(&pool)
    .await?;
    assert_eq!((device_month, user_month), (107, 107));

    let (history_rx, history_tx): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(rx_bytes), 0)::BIGINT,
                COALESCE(SUM(tx_bytes), 0)::BIGINT
           FROM bandwidth_samples WHERE device_id = $1",
    )
    .bind(device)
    .fetch_one(&pool)
    .await?;
    assert_eq!((history_rx, history_tx), (70, 37));

    let (epoch, raw_rx, raw_tx): (i64, i64, i64) = sqlx::query_as(
        "SELECT counter_epoch, raw_server_rx_bytes, raw_server_tx_bytes
           FROM peer_usage_checkpoints WHERE device_id = $1",
    )
    .bind(device)
    .fetch_one(&pool)
    .await?;
    assert_eq!((epoch, raw_rx, raw_tx), (2, 2, 3));
    assert_eq!(
        peer_usage_checkpoints::server_totals(&pool, server).await?,
        (2037, 1070)
    );

    // If the historical sample cannot be written, the checkpoint, totals,
    // and quotas must all roll back together. No partition exists this far
    // in the future in a freshly migrated test database.
    assert!(
        peer_usage_checkpoints::account(
            &pool,
            device,
            user,
            server,
            "test-peer-key",
            "host:gen-1",
            10,
            10,
            now + Duration::days(150),
            true,
        )
        .await
        .is_err()
    );
    let (rx_after_error, tx_after_error): (i64, i64) =
        sqlx::query_as("SELECT lifetime_rx_bytes, lifetime_tx_bytes FROM devices WHERE id = $1")
            .bind(device)
            .fetch_one(&pool)
            .await?;
    assert_eq!((rx_after_error, tx_after_error), (1070, 2037));
    assert_eq!(
        peer_usage_checkpoints::server_totals(&pool, server).await?,
        (2037, 1070)
    );
    Ok(())
}
