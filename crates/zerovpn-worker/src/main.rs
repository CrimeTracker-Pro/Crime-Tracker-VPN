use std::time::Duration;
use std::env;

use anyhow::{Context, Result};
use tokio::signal;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};


use tracing_subscriber::{EnvFilter, fmt, prelude::*};
use zerovpn_db::repos::connection_sessions;
use zerovpn_events::Publisher;
use zerovpn_wire::Event;

mod aggregator;
mod docker_stats;
mod retention;
mod server_health;
mod wg_poller;
mod wg_source;
mod destination_ingest;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    init_tracing();
    info!(version = env!("CARGO_PKG_VERSION"), "zerovpn-worker starting");

    let pub_bind = env::var("ZEROVPN_EVENTS__PUBLISHER_BIND")
        .unwrap_or_else(|_| "tcp://0.0.0.0:5555".to_string());
    let database_url =
        env::var("ZEROVPN_DATABASE_URL").context("ZEROVPN_DATABASE_URL is required")?;

    let pool = zerovpn_db::init_pool(&database_url, 4)
        .await
        .context("connect db")?;
    info!("db connected");

    let mut publisher = Publisher::bind(&pub_bind).await.context("zmq publisher bind")?;

    // Channel funnels events from many tasks to a single publisher task.
    // (zeromq Publisher isn't Sync.)
    let (tx, mut rx) = mpsc::channel::<(String, Event)>(256);

    // Heartbeat task — proves the bus end-to-end and lets the API confirm
    // worker liveness.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                let ts_ms = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
                if tx
                    .send(("events.heartbeat".to_string(), Event::Heartbeat { ts_ms }))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    // Phase 2 / Stage B — close orphaned connection sessions before the
    // poller starts. If the worker died mid-session the row is still
    // open in the DB; the new poller's in-memory `prev_online` map is
    // empty so it won't naturally close them. Sweep them now so the
    // first online observation cleanly opens a fresh session.
    match connection_sessions::close_all_open(&pool).await {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!(rows = n, "closed orphan connection sessions on startup");
        }
        Err(e) => {
            tracing::warn!(?e, "connection_sessions startup sweep failed");
        }
    }

    let wg_source = wg_source::StatsSource::from_env()?;
    if let Some(source) = wg_source.clone() {
        let pool = pool.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            wg_poller::run(pool, tx, source).await;
        });
    } else {
        info!("WG stats disabled: no synthetic chart feed will be emitted until the real WG backend is enabled");
    }

    // Destination ingest: accept JSON flow events and persist them.
    if let Ok(bind) = std::env::var("ZEROVPN_INGEST__DEST_BIND")
        && !bind.is_empty() {
            let pool = pool.clone();
            tokio::spawn(async move {
                if let Err(e) = destination_ingest::run(pool, &bind).await {
                    tracing::error!(?e, "destination ingest failed");
                }
            });
        }

    // Bandwidth aggregator task — rolls up closed hours every 5 minutes
    // and closed days at 00:05 UTC.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            aggregator::run(pool).await;
        });
    }

    // Server-health emitter — every 5s publishes Event::ServerHealth with
    // host CPU/memory/net/disk/uptime. Admin-only via the WS filter.
    {
        let pool = pool.clone();
        let tx = tx.clone();
        let host_source = wg_source.filter(|source| matches!(source, wg_source::StatsSource::HostAgent { .. }));
        tokio::spawn(async move {
            server_health::run(pool, tx, host_source).await;
        });
    }

    // Maintenance task — every 6h creates upcoming monthly partitions,
    // purges expired data, and hard-purges long-deleted accounts.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            retention::run(pool).await;
        });
    }

    // Single publisher task drains the channel onto the ZMQ socket.
    let publisher_task = tokio::spawn(async move {
        while let Some((topic, event)) = rx.recv().await {
            if let Err(e) = publisher.publish(&topic, &event).await {
                warn!(topic, ?e, "publish failed");
            } else {
                debug!(topic, "published");
            }
        }
    });

    shutdown_signal().await;
    publisher_task.abort();
    info!("zerovpn-worker stopped");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,zerovpn=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!(?e, "ctrl_c handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => info!("ctrl-c received"),
        _ = terminate => info!("sigterm received"),
    }
}
