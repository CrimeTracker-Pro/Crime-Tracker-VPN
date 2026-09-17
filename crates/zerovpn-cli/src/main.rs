use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(name = "zerovpn-cli", version, about = "Crime Tracker VPN admin CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run pending DB migrations
    Migrate,
    /// Bootstrap an admin by verified Google email (no password)
    BootstrapAdmin {
        #[arg(long)]
        email: String,
    },
    /// Rotate the WireGuard server keypair (DESTRUCTIVE: invalidates all peer configs).
    ///
    /// With no `--server-id`, rotates the single active server if exactly
    /// one exists. With `--all`, rotates every active server in sequence.
    /// Prompts for confirmation unless `--yes` is passed.
    RotateServerKeys {
        /// Specific server UUID to rotate. Mutually exclusive with `--all`.
        #[arg(long)]
        server_id: Option<uuid::Uuid>,
        /// Rotate every active server.
        #[arg(long, conflicts_with = "server_id")]
        all: bool,
        /// Skip the destructive-action confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Print current version
    Version,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Cmd::Migrate => migrate().await,
        Cmd::BootstrapAdmin { email } => bootstrap_admin(email).await,
        Cmd::RotateServerKeys { server_id, all, yes } => {
            rotate_server_keys(server_id, all, yes).await
        }
        Cmd::Version => {
            println!("zerovpn-cli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().compact())
        .init();
}

async fn migrate() -> Result<()> {
    let database_url = std::env::var("ZEROVPN_DATABASE_URL")?;
    let pool = zerovpn_db::init_pool(&database_url, 4).await?;
    zerovpn_db::run_migrations(&pool).await?;
    println!("migrations applied");
    Ok(())
}

async fn bootstrap_admin(email: String) -> Result<()> {
    let database_url = std::env::var("ZEROVPN_DATABASE_URL")?;
    let pool = zerovpn_db::init_pool(&database_url, 2).await?;
    let email = email.trim().to_lowercase();
    if !email.contains('@') { anyhow::bail!("valid email required"); }
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO users (id, email, password_hash, role, status, must_change_password)
           VALUES ($1, $2, '!', 'admin', 'active', FALSE)
           ON CONFLICT (email) DO UPDATE SET role = 'admin', status = 'active',
               password_hash = '!', must_change_password = FALSE"#,
    )
    .bind(id)
    .bind(&email)
    .execute(&pool)
    .await?;
    println!("admin bootstrapped for Google sign-in: {email}");
    Ok(())
}

/// Rotate the WG server keypair, mirroring the logic in the admin
/// HTTP handler at `routes::admin::rotate_server_keys`:
///   1. mint a new X25519 keypair
///   2. rewrite the server's `wg0.conf` on the shared volume (the WG
///      container reads its interface key from this file)
///   3. persist the new public key in the DB
///
/// After this command, the WG container must be restarted to pick up
/// the new private key, and every peer's `.conf` (which carries the
/// OLD server pubkey) must be re-downloaded.
async fn rotate_server_keys(
    server_id: Option<uuid::Uuid>,
    all: bool,
    yes: bool,
) -> Result<()> {
    let database_url = std::env::var("ZEROVPN_DATABASE_URL")?;
    let pool = zerovpn_db::init_pool(&database_url, 2).await?;

    let targets: Vec<zerovpn_core::models::Server> = if let Some(id) = server_id {
        let s = zerovpn_db::repos::servers::find_by_id(&pool, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("server {id} not found"))?;
        vec![s]
    } else {
        let active = zerovpn_db::repos::servers::list_active(&pool).await?;
        if all {
            active
        } else {
            match active.len() {
                0 => anyhow::bail!("no active servers"),
                1 => active,
                n => anyhow::bail!(
                    "{n} active servers — pass --server-id <uuid> or --all"
                ),
            }
        }
    };

    if !yes {
        let names: Vec<String> = targets
            .iter()
            .map(|s| format!("  {} ({})", s.name, s.id))
            .collect();
        eprintln!(
            "About to rotate WG keypair for {} server(s):\n{}\n\n\
             DESTRUCTIVE: every peer .conf currently references the OLD server\n\
             public key and must be re-downloaded after rotation. The WG\n\
             container must be restarted to pick up the new private key.",
            targets.len(),
            names.join("\n"),
        );
        let proceed = inquire::Confirm::new("Proceed?")
            .with_default(false)
            .prompt()?;
        if !proceed {
            eprintln!("aborted");
            return Ok(());
        }
    }

    let conf_path_base =
        std::env::var("ZEROVPN_WG__SERVER_CONFIG_PATH").unwrap_or_else(|_| "/wg/wg0.conf".into());

    for server in &targets {
        let private = zerovpn_wg::keys::generate_private_key();
        let public = zerovpn_wg::keys::derive_public_key(&private)
            .map_err(|e| anyhow::anyhow!("derive public key: {e}"))?;

        let server_address = format!("{}/{}", server.cidr.network(), server.cidr.prefix());
        let conf = format!(
            "# Auto-generated by zerovpn-cli after key rotation.\n\
             [Interface]\n\
             PrivateKey = {private}\n\
             Address = {server_address}\n\
             ListenPort = {listen_port}\n\
             SaveConfig = false\n\
             PostUp = iptables -A FORWARD -i %i -j ACCEPT; iptables -A FORWARD -o %i -j ACCEPT; iptables -t nat -A POSTROUTING -o eth+ -j MASQUERADE\n\
             PostDown = iptables -D FORWARD -i %i -j ACCEPT; iptables -D FORWARD -o %i -j ACCEPT; iptables -t nat -D POSTROUTING -o eth+ -j MASQUERADE\n",
            listen_port = server.endpoint_port,
        );

        let conf_write_ok = match tokio::fs::write(&conf_path_base, conf.as_bytes()).await {
            Ok(_) => true,
            Err(e) => {
                eprintln!(
                    "warning: wg0.conf rewrite at {conf_path_base} failed: {e}\n\
                     The DB will still be updated; you must place the new\n\
                     PrivateKey into the WG container's wg0.conf manually."
                );
                false
            }
        };

        sqlx::query("UPDATE servers SET public_key = $2 WHERE id = $1")
            .bind(server.id)
            .bind(&public)
            .execute(&pool)
            .await?;

        zerovpn_db::repos::audit::record(
            &pool,
            zerovpn_db::repos::audit::AuditEntry {
                actor_user_id: None,
                action: "cli.server_keys_rotated",
                target_type: Some("server"),
                target_id: Some(server.id),
                metadata: serde_json::json!({
                    "new_public_key": public,
                    "wg0_conf_rewritten": conf_write_ok,
                }),
                ip: None,
            },
        )
        .await?;

        println!(
            "rotated {} ({}):\n  new public_key = {}\n  wg0.conf rewritten = {}",
            server.name, server.id, public, conf_write_ok,
        );
    }

    println!(
        "\nRemember: restart the WG container, and have every peer\n\
         re-download their .conf (the server-side pubkey changed)."
    );
    Ok(())
}
