use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};

use ipnetwork::IpNetwork;
use tokio::{fs, io::AsyncWriteExt};
use tracing::{info, warn};
use zerovpn_auth::kek::Kek;
use zerovpn_db::{
    PgPool,
    repos::{devices, servers},
};
use zerovpn_wg::{WgController, ip_alloc::IpAllocator, keys};

use crate::state::IpAllocators;

/// Resolve `(host, port)` from `ZEROVPN_WG__SERVER_ENDPOINT` (documented as
/// `host:port`) with `ZEROVPN_WG__LISTEN_PORT` as the port fallback. Only a
/// single-colon value is split, so a bare IPv6 literal is left intact.
fn resolve_server_endpoint() -> (String, i32) {
    let raw = std::env::var("ZEROVPN_WG__SERVER_ENDPOINT")
        .unwrap_or_else(|_| "localhost".to_string());
    let listen_port: i32 = std::env::var("ZEROVPN_WG__LISTEN_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(51820);
    if raw.matches(':').count() == 1
        && let Some((host, port)) = raw.split_once(':')
            && let Ok(p) = port.parse::<i32>() {
                return (host.to_string(), p);
            }
    (raw, listen_port)
}

/// Re-add every active device's peer to the running WireGuard interface.
///
/// The interface is recreated empty whenever the WG container/process
/// restarts, while runtime peer adds otherwise only happen on device
/// create / key-rotate / unpause. Without this boot-time pass a restart
/// silently drops every tunnel until each device is touched. Idempotent —
/// `wg set peer` and the kernel `configure_peer` are upserts — so it is safe
/// to run on every api start (including hot-reload restarts). No-op on the
/// noop backend and when there are no active devices.
pub async fn reconcile_peers(pool: &PgPool, wg: &Arc<dyn WgController>) -> anyhow::Result<()> {
    // JOIN to pick up each peer's home server's PersistentKeepalive so the live
    // WG interface state matches the per-server setting (default 30s if the
    // column is NULL, though the migration sets a NOT NULL default).
    let rows: Vec<(String, IpNetwork, i16)> = sqlx::query_as(
        "SELECT d.public_key, d.allocated_ip, s.persistent_keepalive
           FROM devices d
           JOIN servers s ON s.id = d.server_id
          WHERE d.status = 'active'",
    )
    .fetch_all(pool)
    .await?;
    let total = rows.len();
    if total == 0 {
        return Ok(());
    }
    let mut restored = 0usize;
    for (public_key, allocated_ip, keepalive) in rows {
        match wg
            .add_peer(&public_key, allocated_ip.ip(), None, keepalive as u16)
            .await
        {
            Ok(()) => restored += 1,
            Err(e) => warn!(?e, %public_key, "reconcile_peers: add_peer failed"),
        }
    }
    info!(restored, total, "reconciled active peers onto WG interface");
    Ok(())
}

/// Run a command to completion, capturing output; returns whether it exited 0.
/// Used for best-effort WG interface bring-up / iptables — failures are logged
/// by the caller, never fatal.
async fn run(cmd: &str, args: &[&str]) -> bool {
    match tokio::process::Command::new(cmd).args(args).output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Path `wg0.conf` is written to (shared `wg_config` volume). Configurable so it
/// matches wherever the WG image reads its config from.
fn server_conf_path() -> PathBuf {
    std::env::var("ZEROVPN_WG__SERVER_CONFIG_PATH")
        .unwrap_or_else(|_| "/wg/wg0.conf".to_string())
        .into()
}

/// Read the `PrivateKey` value from an existing `wg0.conf`, if the file exists
/// and contains one. Used to adopt a dev-entrypoint-generated key and to
/// backfill legacy server rows that predate DB-stored keys.
async fn read_conf_private_key(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).await.ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.trim().strip_prefix("PrivateKey") {
            let key = rest.trim_start_matches([' ', '=']).trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// (Re)write `wg0.conf` for the default server from `private_key`. Idempotent —
/// safe on every boot; only the interface bring-up consumes the file.
async fn write_server_conf(private_key: &str, listen_port: i32, cidr: IpNetwork) {
    let conf_path = server_conf_path();
    let interface = std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into());
    let server_address = match cidr {
        IpNetwork::V4(network) => {
            let first_host = Ipv4Addr::from(u32::from(network.network()) + 1);
            format!("{first_host}/{}", network.prefix())
        }
        // IPv6 server networks are not currently provisioned, but retain the
        // configured prefix if one is added rather than falling back to a
        // hard-coded IPv4 range.
        IpNetwork::V6(network) => format!("{}/{}", network.network(), network.prefix()),
    };
    // No PostUp/PostDown: forwarding/NAT/DNS-DNAT is applied best-effort by
    // `ensure_wg_interface_up` after bring-up, so a failing iptables rule can't
    // make wg-quick roll the whole interface back.
    let conf = format!(
        "# Auto-generated by zerovpn-api from the DB-stored server key. Editing\n\
         # is pointless — it is rewritten from `servers.private_key_encrypted`\n\
         # on boot. Delete the server row in the DB to force a new key.\n\
         [Interface]\n\
         PrivateKey = {private_key}\n\
         Address = {server_address}\n\
         ListenPort = {listen_port}\n\
         SaveConfig = false\n",
    );

    if let Some(parent) = conf_path.parent()
        && let Err(e) = fs::create_dir_all(parent).await {
            warn!(?e, parent = %parent.display(), "could not create wg config dir");
        }
    match fs::File::create(&conf_path).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(conf.as_bytes()).await {
                warn!(?e, path = %conf_path.display(), "wg0.conf write failed");
            } else {
                let _ = f.sync_all().await;
                info!(interface, path = %conf_path.display(), "wg0.conf written from DB key");
            }
        }
        Err(e) => {
            // Likely the volume isn't mounted (e.g., dev demo without WG
            // container). Log + continue; api still works for the rest of the
            // stack and ShellController will be a no-op anyway.
            warn!(
                ?e,
                path = %conf_path.display(),
                "could not write wg0.conf — WG container will need it injected manually"
            );
        }
    }
}

/// Tear down and (re)bring-up `iface` from `conf_str`, then apply best-effort
/// forwarding / NAT. The clean-slate down/del makes it safe to call
/// on an interface that's already up (e.g. after a server key rotation).
async fn bring_up_wg(iface: &str, conf_str: &str) {
    let _ = run("wg-quick", &["down", conf_str]).await;
    let _ = run("ip", &["link", "del", iface]).await;
    if !run("wg-quick", &["up", conf_str]).await {
        warn!(interface = %iface, "wg-quick up failed — api runs, but no live tunnel");
        return;
    }
    info!(interface = %iface, "wg interface up");

    // Best-effort forwarding + NAT for full-tunnel egress. Non-fatal — split
    // tunnel to the VPN subnet needs none of this.
    let _ = run("sysctl", &["-w", "net.ipv4.ip_forward=1"]).await;
    let _ = run("iptables", &["-A", "FORWARD", "-i", iface, "-j", "ACCEPT"]).await;
    let _ = run("iptables", &["-A", "FORWARD", "-o", iface, "-j", "ACCEPT"]).await;
    let _ = run(
        "iptables",
        &["-t", "nat", "-A", "POSTROUTING", "-o", "eth0", "-j", "MASQUERADE"],
    )
    .await;

}

/// Bring the WG interface up from `wg0.conf`. This is what makes the api the WG
/// host directly (no separate `wg` container). No-op on the `noop` backend.
/// **Idempotent** — the interface is a kernel object in this container's netns
/// and survives an api *process* restart, so a hot-reload does not flap the
/// tunnel (we skip when it's already up).
pub async fn ensure_wg_interface_up() {
    if std::env::var("ZEROVPN_WG__BACKEND").as_deref() == Ok("noop") {
        return;
    }
    let iface = std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into());
    // `wg show <iface>` exits 0 only for a live WG interface — skip if already up.
    if run("wg", &["show", &iface]).await {
        info!(interface = %iface, "wg interface already up; leaving it in place");
        return;
    }
    bring_up_wg(&iface, &server_conf_path().to_string_lossy()).await;
}

/// Force-reapply the interface from the (freshly rewritten) `wg0.conf` — used
/// after a server key rotation so the new key takes effect on the live tunnel.
pub async fn reapply_wg_interface() {
    if std::env::var("ZEROVPN_WG__BACKEND").as_deref() == Ok("noop") {
        return;
    }
    let iface = std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into());
    bring_up_wg(&iface, &server_conf_path().to_string_lossy()).await;
}

/// Ensure the `default` server row exists AND that its private key lives in the
/// DB (KEK-encrypted). The DB is the source of truth: on boot we restore
/// `wg0.conf` from the stored key, so the `wg_config` volume is only a derived
/// cache — losing it no longer loses (or silently rotates) the server key, and
/// the whole stack recovers from Postgres alone.
pub async fn ensure_default_server(pool: &PgPool, kek: &Kek) -> anyhow::Result<()> {
    let (endpoint_host, listen_port) = resolve_server_endpoint();
    let conf_path = server_conf_path();

    if let Some((id, enc, cidr)) = servers::default_key_state(pool).await? {
        // Dev convenience: re-sync the default server's endpoint from the
        // configured value on each boot so a changing Wi-Fi/LAN IP is picked
        // up automatically. In prod we leave admin-edited endpoints untouched.
        if std::env::var("ZEROVPN_ENVIRONMENT").as_deref() == Ok("dev") {
            let res = sqlx::query(
                "UPDATE servers SET endpoint_host = $1, endpoint_port = $2 \
                 WHERE name = 'default'",
            )
            .bind(&endpoint_host)
            .bind(listen_port)
            .execute(pool)
            .await?;
            if res.rows_affected() > 0 {
                info!(
                    endpoint = %format!("{endpoint_host}:{listen_port}"),
                    "dev: synced default server endpoint from env"
                );
            }
        }

        // Resolve the authoritative private key: DB first, then backfill a
        // legacy row from the live wg0.conf, then (last resort) mint a new one.
        let private_key = match enc {
            Some(enc) => String::from_utf8(kek.decrypt(&enc)?)
                .map_err(|e| anyhow::anyhow!("server private key not utf-8: {e}"))?,
            None => match read_conf_private_key(&conf_path).await {
                Some(pk) => {
                    servers::set_private_key(pool, id, &kek.encrypt(pk.as_bytes())?).await?;
                    info!("backfilled server private key into the DB from wg0.conf");
                    pk
                }
                None => {
                    warn!(
                        "no server key in the DB or wg0.conf; minting a new one — \
                         existing client configs will need re-download"
                    );
                    let pk = keys::generate_private_key();
                    let pubkey = keys::derive_public_key(&pk)?;
                    servers::set_keypair(pool, id, &pubkey, &kek.encrypt(pk.as_bytes())?).await?;
                    pk
                }
            },
        };

        // Rebuild the derived config from the DB on every boot.  Besides the
        // key, it contains the server CIDR; keeping it in sync makes a direct
        // CIDR correction take effect on the next service restart.
        write_server_conf(&private_key, listen_port, cidr).await;
        info!(server_id = %id, "restored wg0.conf from the DB-stored server record");
        return Ok(());
    }

    info!("no servers found; creating default");

    // Adopt an existing wg0.conf key (e.g. one the dev entrypoint generated to
    // bring the interface up before the api started) so the DB matches the live
    // interface instead of minting a competing key; else generate a fresh one.
    let private_key = match read_conf_private_key(&conf_path).await {
        Some(pk) => pk,
        None => keys::generate_private_key(),
    };
    let public_key = keys::derive_public_key(&private_key)?;
    let encrypted = kek.encrypt(private_key.as_bytes())?;

    let cidr: IpNetwork = "10.10.0.0/22".parse().unwrap();
    let dns: IpNetwork = "10.10.0.1/32".parse().unwrap();

    let id = servers::create(
        pool,
        servers::NewServer {
            name: "default",
            region: "local",
            endpoint_host: &endpoint_host,
            endpoint_port: listen_port,
            public_key: &public_key,
            private_key_encrypted: &encrypted,
            cidr,
            dns_servers: vec![dns],
            mtu: 1420,
        },
    )
    .await?;

    if read_conf_private_key(&conf_path).await.as_deref() != Some(private_key.as_str()) {
        write_server_conf(&private_key, listen_port, cidr).await;
    }

    info!(server_id = %id, public_key = %public_key, "default server created");
    Ok(())
}

/// Build per-server IP allocators from the DB on startup. The allocator
/// dispatches internally on the network family — both IPv4 and IPv6
/// servers are seeded with their existing allocations from the
/// `devices` table.
pub async fn build_ip_allocators(pool: &PgPool) -> anyhow::Result<Arc<IpAllocators>> {
    let allocators = Arc::new(IpAllocators::default());
    for s in servers::list_active(pool).await? {
        let alloc = Arc::new(IpAllocator::new(s.cidr));
        for ip in devices::allocated_ips_for_server(pool, s.id).await? {
            if let Err(e) = alloc.mark_allocated(ip) {
                warn!(server_id = %s.id, %ip, err = ?e, "failed to seed allocator");
            }
        }
        allocators.insert(s.id, alloc);
    }
    info!(servers = allocators.map.read().len(), "ip allocators built");
    Ok(allocators)
}
