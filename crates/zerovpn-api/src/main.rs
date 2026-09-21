use std::{env, net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use tokio::signal;
use tower_http::{
    compression::CompressionLayer, cors::CorsLayer, request_id::SetRequestIdLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};
use zerovpn_events::Subscriber;
use zerovpn_core::models::{UserRole, UserStatus};
use zerovpn_db::repos::users;
use zerovpn_wire::Event;

mod bootstrap;
mod error;
mod extractors;
mod middleware;
mod quota;
mod ratelimit;
mod routes;
mod state;

use state::AppState;

/// Parse a boolean-ish env var. Returns `None` when unset/empty/unrecognized so
/// callers can distinguish "not configured" from an explicit true/false.
fn env_bool(key: &str) -> Option<bool> {
    match env::var(key).ok()?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Session idle window in minutes. `ZEROVPN_SESSION_IDLE_MINUTES` wins when
/// set to a positive integer; otherwise 7 days in production, 30 in dev.
fn session_idle_minutes(is_production: bool) -> i64 {
    env::var("ZEROVPN_SESSION_IDLE_MINUTES")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|m: &i64| *m > 0)
        .unwrap_or(if is_production { 7 * 24 * 60 } else { 30 * 24 * 60 })
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    init_tracing();
    if let Err(e) = routes::metrics::install_global_recorder() {
        warn!(?e, "prometheus recorder install failed; /metrics will return 503");
    }

    let is_production = env::var("ZEROVPN_ENVIRONMENT").as_deref() == Ok("production");
    validate_production_config(is_production)?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        environment = if is_production { "production" } else { "dev" },
        "zerovpn-api starting"
    );

    let bind_addr =
        env::var("ZEROVPN_BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let database_url =
        env::var("ZEROVPN_DATABASE_URL").context("ZEROVPN_DATABASE_URL is required")?;

    let pool = zerovpn_db::init_pool(&database_url, 16)
        .await
        .context("connect db")?;
    info!("db connected");

    // Run app migrations on startup. Idempotent.
    zerovpn_db::run_migrations(&pool)
        .await
        .context("run migrations")?;

    // The initial administrator is explicitly configured and can only sign
    // in through a Google account with this verified email. No password or
    // public registration path exists.
    let bootstrap_email = env::var("ZEROVPN_BOOTSTRAP_ADMIN_EMAIL").unwrap_or_default();
    let bootstrap_email = bootstrap_email.trim().to_lowercase();
    if users::count_active_admins(&pool).await? == 0 {
        if bootstrap_email.is_empty() {
            anyhow::bail!("ZEROVPN_BOOTSTRAP_ADMIN_EMAIL is required when no active admin exists");
        }
        let email = bootstrap_email;
        if let Some(existing) = users::find_by_email(&pool, &email).await? {
            sqlx::query("UPDATE users SET role = 'admin', status = 'active', must_change_password = FALSE, password_hash = '!' WHERE id = $1")
                .bind(existing.id).execute(&pool).await?;
        } else {
            users::create(&pool, &email, "!", UserRole::Admin, UserStatus::Active).await?;
        }
        info!(email, "bootstrap admin ready for Google sign-in");
    }

    // Tower-sessions session store + its own migration.
    let session_store = PostgresStore::new(pool.clone());
    session_store.migrate().await.context("session store migrate")?;
    // `secure: true` requires the cookie to ride only over HTTPS. Traefik
    // terminates TLS for us in production; in dev we serve over plaintext
    // localhost so the flag is off.
    // Session idle window — every authenticated request refreshes it, so
    // this is "signed out after N of no activity", not an absolute cap.
    // Operators tune it via ZEROVPN_SESSION_IDLE_MINUTES; defaults are
    // 7 days in production and 30 days in dev (so a cargo restart or an
    // overnight tab doesn't kick you back to /login).
    let idle_expiry = time::Duration::minutes(session_idle_minutes(is_production));
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(is_production)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_name("zerovpn_session")
        .with_expiry(Expiry::OnInactivity(idle_expiry));

    let kek_b64 = env::var("ZEROVPN_KEK").context("ZEROVPN_KEK is required")?;
    let kek = zerovpn_auth::kek::Kek::from_b64(&kek_b64)
        .map_err(|e| anyhow::anyhow!("invalid KEK: {e}"))?;

    // The WG server keypair lives in the DB (KEK-encrypted); this creates or
    // loads it and restores wg0.conf from it, so the wg_config volume is only a
    // derived cache (the api holds no unique state).
    bootstrap::ensure_default_server(&pool, &kek)
        .await
        .context("bootstrap server")?;
    // Bring wg0 up from the just-written config (the api is the WG host now —
    // no separate `wg` container). No-op on the noop backend; idempotent across
    // hot-reload restarts. Runs before reconcile_peers so peers land on a live
    // interface.
    bootstrap::ensure_wg_interface_up().await;
    let allocators = bootstrap::build_ip_allocators(&pool)
        .await
        .context("build ip allocators")?;


    let public_url =
        env::var("ZEROVPN_PUBLIC_URL").unwrap_or_else(|_| "http://localhost".into());

    // Build a Mailer if SMTP is configured; otherwise routes fall back to
    // logging the link so dev still works without MailHog.
    let mailer = match (env::var("ZEROVPN_SMTP__HOST"), env::var("ZEROVPN_SMTP__PORT")) {
        (Ok(host), Ok(port_str)) if !host.is_empty() => {
            let port: u16 = port_str.parse().unwrap_or(1025);
            let user = env::var("ZEROVPN_SMTP__USERNAME").ok().filter(|s| !s.is_empty());
            let pass = env::var("ZEROVPN_SMTP__PASSWORD").ok().filter(|s| !s.is_empty());
            let from_str = env::var("ZEROVPN_SMTP__FROM")
                .unwrap_or_else(|_| "Crime Tracker VPN <noreply@localhost>".into());
            let from: zerovpn_mail::Mailbox = from_str
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid SMTP_FROM: {e}"))?;
            // TLS mode is explicit from .env (ZEROVPN_SMTP__SSL_TLS / __STARTTLS).
            // SSL_TLS (implicit TLS) wins if both are true; if neither var is set
            // at all we fall back to the port convention (465=SSL, 1025=none,
            // else STARTTLS) so a HOST+PORT-only config still works.
            let ssl_tls = env_bool("ZEROVPN_SMTP__SSL_TLS");
            let starttls = env_bool("ZEROVPN_SMTP__STARTTLS");
            let encryption = if ssl_tls == Some(true) {
                zerovpn_mail::SmtpEncryption::Ssl
            } else if starttls == Some(true) {
                zerovpn_mail::SmtpEncryption::StartTls
            } else if ssl_tls.is_some() || starttls.is_some() {
                zerovpn_mail::SmtpEncryption::None
            } else {
                match port {
                    465 => zerovpn_mail::SmtpEncryption::Ssl,
                    1025 => zerovpn_mail::SmtpEncryption::None,
                    _ => zerovpn_mail::SmtpEncryption::StartTls,
                }
            };
            let validate_certs = env_bool("ZEROVPN_SMTP__VALIDATE_CERTS").unwrap_or(true);
            match zerovpn_mail::Mailer::new(
                &host,
                port,
                user.as_deref(),
                pass.as_deref(),
                from,
                encryption,
                validate_certs,
            ) {
                Ok(m) => {
                    info!(host, port, ?encryption, validate_certs, "smtp configured");
                    Some(m)
                }
                Err(e) => {
                    warn!(?e, "smtp init failed; mail send will fall back to log");
                    None
                }
            }
        }
        _ => {
            info!("ZEROVPN_SMTP__HOST not set; mail disabled (dev mode)");
            None
        }
    };

    let wg_controller = zerovpn_wg::control::from_env();

    // Google OAuth is optional — when any of the three vars is missing the
    // /auth/google/* routes return 503 and the rest of the API boots fine.
    let google_oauth = match (
        env::var("ZEROVPN_GOOGLE_OAUTH__CLIENT_ID").ok().filter(|s| !s.is_empty()),
        env::var("ZEROVPN_GOOGLE_OAUTH__CLIENT_SECRET").ok().filter(|s| !s.is_empty()),
        env::var("ZEROVPN_GOOGLE_OAUTH__REDIRECT_URL").ok().filter(|s| !s.is_empty()),
    ) {
        (Some(client_id), Some(client_secret), Some(redirect_url)) => {
            info!(redirect_url, "google oauth configured");
            Some(zerovpn_core::config::GoogleOAuthConfig {
                client_id,
                client_secret,
                redirect_url,
            })
        }
        _ => {
            info!("ZEROVPN_GOOGLE_OAUTH__* not set; google sign-in disabled");
            None
        }
    };

    let app_state = AppState::new(
        pool,
        allocators,
        kek,
        mailer,
        public_url,
        wg_controller,
        google_oauth,
    );

    // Re-add active peers to the (possibly freshly recreated) WG interface so
    // existing tunnels keep working across restarts without re-creating each
    // device. Idempotent; best-effort.
    if let Err(e) = bootstrap::reconcile_peers(&app_state.pool, &app_state.wg).await {
        warn!(?e, "startup peer reconcile failed");
    }

    // Background quota-enforcement sweep: resets elapsed monthly windows,
    // auto-resumes devices it had paused, and auto-pauses devices over the
    // per-device or account cap. Owns peer add/remove (via state.wg), which the
    // worker can't do.
    quota::spawn(app_state.clone());

    // ZMQ subscriber: consumes worker events (subscribed to all topics) and
    // forwards them onto the in-process broadcast bus that WS handlers tap.
    if let Ok(sub_url) = env::var("ZEROVPN_EVENTS__SUBSCRIBER_CONNECT") {
        let bus = app_state.events.clone();
        tokio::spawn(async move {
            let mut delay = Duration::from_millis(250);
            let sub = loop {
                match Subscriber::connect(&sub_url, "").await {
                    Ok(s) => break s,
                    Err(e) => {
                        warn!(?e, "zmq subscriber connect failed, retrying");
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(10));
                    }
                }
            };
            run_subscriber(sub, bus).await;
        });
    } else {
        info!("ZEROVPN_EVENTS__SUBSCRIBER_CONNECT not set; skipping ZMQ subscriber");
    }

    let app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/ready", get(routes::health::ready))
        .route("/metrics", get(routes::metrics::metrics))
        .route("/openapi.json", get(routes::openapi::spec))
        .nest(
            "/api/v1",
            Router::new()
                .route("/ping", get(routes::health::ping))
                .route("/auth/logout", post(routes::auth::logout))
                .route("/auth/google/start", get(routes::oauth::google_start))
                .route("/auth/google/callback", post(routes::oauth::google_callback))
                .route(
                    "/auth/google/verify-totp",
                    post(routes::oauth::google_verify_totp),
                )
                .route("/me", get(routes::auth::me))
                .route(
                    "/connections",
                    get(routes::connections::list_for_user),
                )
                .route(
                    "/devices",
                    get(routes::devices::list).post(routes::devices::create),
                )
                // Bulk-reorder must come BEFORE `/devices/{id}` so axum's
                // matcher doesn't treat "order" as a uuid path segment.
                .route(
                    "/devices/order",
                    axum::routing::put(routes::devices::reorder),
                )
                // App one-tap connect/provision. Static path, must precede
                // the `/devices/{id}` catch-all so "connect" isn't parsed as
                // a device UUID.
                .route(
                    "/devices/connect",
                    post(routes::devices::connect),
                )
                .route(
                    "/devices/{id}",
                    get(routes::devices::get)
                        .delete(routes::devices::delete)
                        .patch(routes::devices::patch),
                )
                .route("/devices/{id}/pause", post(routes::devices::pause))
                .route("/devices/{id}/unpause", post(routes::devices::unpause))
                .route(
                    "/devices/{id}/rotate-keys",
                    post(routes::devices::rotate_keys),
                )
                .route(
                    "/devices/{id}/conf",
                    get(routes::devices::redownload_conf),
                )
                .route(
                    "/devices/{id}/quota",
                    axum::routing::put(routes::devices::set_my_quota),
                )
                .route("/devices/{id}/events", get(routes::devices::events))
                .route("/devices/{id}/bandwidth", get(routes::bandwidth::for_device))
                .route("/devices/{id}/history", get(routes::bandwidth::device_history))
                .route("/devices/{id}/candles", get(routes::bandwidth::device_candles))
                .route("/servers/{id}/history", get(routes::bandwidth::server_history))
                .route("/servers/{id}/candles", get(routes::bandwidth::server_candles))
                .route("/candles", get(routes::bandwidth::user_candles))
                .route("/bandwidth", get(routes::bandwidth::for_user))
                .route("/auth/totp/setup", post(routes::totp::setup))
                .route("/auth/totp/enable", post(routes::totp::enable))
                .route("/auth/totp/disable", post(routes::totp::disable))
                .route(
                    "/auth/totp/recovery-codes",
                    post(routes::totp::regenerate_recovery_codes),
                )
                .route("/me/data-export", get(routes::me::export))
                .route("/me/server", get(routes::me::server_info))
                .route("/me/usage", get(routes::me::usage))
                .route("/me/activity", get(routes::me::activity))
                .route(
                    "/me/topology",
                    get(routes::me::get_topology).put(routes::me::set_topology),
                )
                .route(
                    "/me/account",
                    axum::routing::delete(routes::me::delete_account),
                )
                .route("/me/sessions", get(routes::me::list_sessions))
                .route(
                    "/me/sessions/{id}",
                    axum::routing::delete(routes::me::revoke_session),
                )
                .route(
                    "/me/sessions/revoke-all",
                    post(routes::me::revoke_other_sessions),
                )
                .route(
                    "/me/preferences",
                    get(routes::me::get_preferences).put(routes::me::set_preferences),
                )
                .route("/admin/stats", get(routes::admin::stats))
                .route("/admin/bandwidth", get(routes::admin::fleet_bandwidth))
                .route(
                    "/admin/users",
                    get(routes::admin::list_users).post(routes::admin::create_user),
                )
                .route("/admin/users.csv", get(routes::admin::list_users_csv))
                .route(
                    "/admin/users/{id}",
                    get(routes::admin::user_detail).delete(routes::admin::delete_user),
                )
                .route(
                    "/admin/users/{id}/status",
                    axum::routing::put(routes::admin::set_user_status),
                )
                .route(
                    "/admin/users/{id}/role",
                    axum::routing::put(routes::admin::set_user_role),
                )
                .route(
                    "/admin/users/{id}/disable-2fa",
                    post(routes::admin::admin_disable_2fa),
                )
                .route(
                    "/admin/users/{id}/sessions/revoke-all",
                    post(routes::admin::admin_revoke_sessions),
                )
                .route(
                    "/admin/users/{id}/email",
                    axum::routing::put(routes::admin::admin_set_email_route),
                )
                .route(
                    "/admin/users/{id}/bandwidth",
                    get(routes::admin::user_bandwidth),
                )
                .route(
                    "/admin/users/{id}/candles",
                    get(routes::bandwidth::admin_user_candles),
                )
                .route("/admin/audit", get(routes::admin::list_audit))
                .route("/admin/audit.csv", get(routes::admin::list_audit_csv))
                .route("/admin/failed-logins", get(routes::admin::list_failed_logins))
                .route(
                    "/admin/session-events",
                    get(routes::admin::list_session_events),
                )
                .route(
                    "/admin/access-logs",
                    get(routes::admin::list_access_logs),
                )
                .route("/admin/finder", get(routes::admin::finder))
                .route(
                    "/admin/maintenance",
                    get(routes::admin::get_maintenance).put(routes::admin::set_maintenance),
                )
                .route(
                    "/admin/user-policy",
                    get(routes::admin::get_user_policy).put(routes::admin::set_user_policy),
                )
                .route(
                    "/admin/connections",
                    get(routes::connections::list_all),
                )
                .route(
                    "/admin/users/{id}/quota",
                    axum::routing::put(routes::admin::set_user_quota),
                )
                .route(
                    "/admin/users/{id}/device-limit",
                    axum::routing::put(routes::admin::set_user_device_limit),
                )
                .route("/admin/devices", get(routes::admin::list_devices))
                .route(
                    "/admin/devices/{id}",
                    get(routes::admin::device_detail).delete(routes::admin::admin_revoke_device),
                )
                .route(
                    "/admin/devices/{id}/pause",
                    post(routes::admin::admin_pause_device),
                )
                .route(
                    "/admin/devices/{id}/unpause",
                    post(routes::admin::admin_unpause_device),
                )
                .route(
                    "/admin/devices/{id}/bandwidth",
                    get(routes::admin::device_bandwidth),
                )
                .route(
                    "/admin/devices/{id}/candles",
                    get(routes::bandwidth::admin_device_candles),
                )
                .route(
                    "/admin/devices/{id}/quota",
                    axum::routing::put(routes::admin::set_device_quota),
                )
                .route(
                    "/admin/devices/{id}/endpoint-history",
                    get(routes::admin::device_endpoint_history),
                )
                .route(
                    "/admin/devices/{id}/connection-history",
                    get(routes::admin::device_connection_history),
                )
                .route("/admin/servers", get(routes::admin::list_servers))
                .route(
                    "/admin/servers/{id}",
                    get(routes::admin::server_detail)
                        .patch(routes::admin::patch_server),
                )
                .route(
                    "/admin/servers/{id}/bandwidth",
                    get(routes::admin::server_bandwidth),
                )
                .route(
                    "/admin/servers/{id}/rotate-keys",
                    post(routes::admin::rotate_server_keys),
                )
                .route(
                    "/admin/users/{id}/impersonate",
                    post(routes::admin::impersonate_user),
                )
                .route(
                    "/admin/impersonate/stop",
                    post(routes::admin::stop_impersonation),
                )
                .route("/auth/invitations/verify", post(routes::invitations::verify))
                .route("/admin/invitations", get(routes::invitations::list))
                .route("/admin/invitations/{id}/resend", post(routes::invitations::resend))
                .route("/admin/invitations/{id}/link", post(routes::invitations::create_link))
                .route("/admin/invitations/{id}/revoke", post(routes::invitations::revoke))
                .route("/ws", get(routes::ws::ws)),
        )
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            middleware::maintenance_gate,
        ))
        // Phase 2 / Stage B — per-request access log. Slotted INSIDE the
        // session layer (so `req.extensions::<Session>()` is populated)
        // and OUTSIDE the maintenance_gate (so a maintenance-mode 503
        // is still logged, with status 503). SetRequestIdLayer is even
        // further out, so the `x-request-id` header is already set.
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            middleware::access_log,
        ))
        .layer(session_layer)
        .with_state(app_state)
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(SetRequestIdLayer::x_request_id(
            tower_http::request_id::MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = bind_addr.parse().context("parse bind address")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    info!("zerovpn-api stopped");
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

/// Refuse to boot in production with default / weak / dev-style config.
/// Catches the "you forgot to swap `.env.dev` for `.env.prod`" failure mode
/// before the api ever binds a port.
fn validate_production_config(is_production: bool) -> Result<()> {
    if !is_production {
        return Ok(());
    }

    let session = env::var("ZEROVPN_SESSION_SECRET").unwrap_or_default();
    if session == "CHANGEME" || session.len() < 32 {
        anyhow::bail!(
            "production: ZEROVPN_SESSION_SECRET must be a 32+ byte base64 random; \
             run ./scripts/init-secrets.sh prod"
        );
    }

    let kek = env::var("ZEROVPN_KEK").unwrap_or_default();
    if kek == "CHANGEME" || kek.len() < 32 {
        anyhow::bail!(
            "production: ZEROVPN_KEK must be a 32-byte base64 random; \
             run ./scripts/init-secrets.sh prod"
        );
    }

    let domain = env::var("ZEROVPN_DOMAIN").unwrap_or_default();
    if domain.is_empty()
        || domain == "localhost"
        || domain.starts_with("REPLACE")
    {
        anyhow::bail!(
            "production: ZEROVPN_DOMAIN must be a real public domain (got '{}'); \
             Traefik needs it for Let's Encrypt issuance",
            domain
        );
    }

    let smtp_host = env::var("ZEROVPN_SMTP__HOST").unwrap_or_default();
    if smtp_host == "mailhog" || smtp_host.starts_with("smtp.REPLACE") {
        anyhow::bail!(
            "production: ZEROVPN_SMTP__HOST is still pointing at a dev / placeholder \
             relay ('{}'); set a real SMTP host or unset to disable mail",
            smtp_host
        );
    }

    Ok(())
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
            Err(e) => {
                warn!(?e, "sigterm handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => info!("ctrl-c received"),
        _ = terminate => info!("sigterm received"),
    }
}

async fn run_subscriber(mut sub: Subscriber, bus: tokio::sync::broadcast::Sender<Event>) {
    info!("zmq subscriber loop started");
    loop {
        match sub.recv().await {
            Ok((topic, event)) => {
                debug!(topic, kind = event_kind(&event), "event received");
                // No receivers? Fine — broadcast just drops. The next WS
                // client that connects starts seeing fresh events.
                let _ = bus.send(event);
            }
            Err(e) => {
                warn!(?e, "zmq subscriber recv error");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::Heartbeat { .. } => "heartbeat",
        Event::StatsDelta { .. } => "stats_delta",
        Event::HandshakeChange { .. } => "handshake_change",
        Event::PeerStatusChanged { .. } => "peer_status_changed",
        Event::ServerHealth { .. } => "server_health",
        Event::ServerSample { .. } => "server_sample",
        Event::DataChanged { .. } => "data_changed",
        Event::Notify { .. } => "notify",
    }
}

#[cfg(test)]
mod session_idle_tests {
    use super::session_idle_minutes;

    // Single test so the env-var mutation can't race a parallel case.
    #[test]
    fn env_override_and_defaults() {
        const KEY: &str = "ZEROVPN_SESSION_IDLE_MINUTES";
        unsafe { std::env::remove_var(KEY) };
        assert_eq!(session_idle_minutes(true), 7 * 24 * 60);
        assert_eq!(session_idle_minutes(false), 30 * 24 * 60);
        unsafe { std::env::set_var(KEY, "10080") };
        assert_eq!(session_idle_minutes(true), 10080);
        // Garbage / non-positive values fall back to the defaults.
        unsafe { std::env::set_var(KEY, "0") };
        assert_eq!(session_idle_minutes(true), 7 * 24 * 60);
        unsafe { std::env::set_var(KEY, "soon") };
        assert_eq!(session_idle_minutes(false), 30 * 24 * 60);
        unsafe { std::env::remove_var(KEY) };
    }
}
