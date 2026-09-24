//! Database-to-host reconciliation. The API never mutates host WireGuard directly.
//! A database transaction serializes snapshots and records pending/applied state.

use std::{net::IpAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use tokio::time::{interval, timeout};
use tracing::{error, info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;
use zerovpn_auth::kek::Kek;
use zerovpn_db::PgPool;
use zerovpn_host_agent::{Operation, PROTOCOL_VERSION, Response, query,
    secret_file::load_agent_token, state::{DesiredPeer, DesiredState}};
use zerovpn_wg::{WgController, control::ControlError};

const AGENT_TIMEOUT: Duration = Duration::from_secs(90);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

pub struct HostAgentController {
    pool: PgPool,
    kek: Arc<Kek>,
    socket: PathBuf,
    read_token: Zeroizing<String>,
    apply_token: Zeroizing<String>,
}

impl HostAgentController {
    pub fn from_env(pool: PgPool, kek: Arc<Kek>) -> Result<Self> {
        let socket = required_absolute_path("ZEROVPN_API__HOST_AGENT_SOCKET")?;
        let read_token = load_agent_token(&required_absolute_path("ZEROVPN_API__HOST_AGENT_READ_TOKEN_FILE")?)?;
        let apply_token = load_agent_token(&required_absolute_path("ZEROVPN_API__HOST_AGENT_APPLY_TOKEN_FILE")?)?;
        ensure!(read_token.as_str() != apply_token.as_str(), "host agent tokens must differ");
        Ok(Self { pool, kek, socket, read_token, apply_token })
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut tick = interval(RECONCILE_INTERVAL);
            loop {
                tick.tick().await;
                if let Err(error) = self.reconcile().await {
                    warn!(%error, "host-agent desired-state reconciliation failed; will retry");
                }
            }
        });
    }

    async fn request(&self, token: &str, operation: Operation) -> Result<Response> {
        timeout(AGENT_TIMEOUT, query(&self.socket, token, operation))
            .await.context("host agent timed out")?
    }

    /// The advisory transaction lock covers DB snapshot, agent apply, and
    /// status commit, including across multiple API processes.
    pub async fn reconcile(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(862181921409::bigint)")
            .execute(&mut *tx).await?;
        let servers: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, public_key FROM servers WHERE is_active = TRUE ORDER BY created_at")
            .fetch_all(&mut *tx).await?;
        if servers.is_empty() { return Ok(()); }
        ensure!(servers.len() == 1, "host agent requires exactly one active server");
        let (server_id, server_public_key) = &servers[0];
        sqlx::query("INSERT INTO host_agent_reconcile (server_id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(server_id).execute(&mut *tx).await?;
        let row: (i64, String, String, i64, String, String) = sqlx::query_as(
            "SELECT desired_revision, desired_content_hash, desired_digest, \
                    applied_revision, applied_digest, status \
               FROM host_agent_reconcile WHERE server_id = $1 FOR UPDATE")
            .bind(server_id).fetch_one(&mut *tx).await?;
        let rows: Vec<(String, IpNetwork, i16, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT d.public_key, d.allocated_ip, s.persistent_keepalive, d.preshared_key_encrypted \
               FROM devices d JOIN servers s ON s.id = d.server_id \
               JOIN users u ON u.id = d.user_id \
              WHERE d.server_id = $1 AND d.status = 'active' AND u.status = 'active' \
              ORDER BY d.public_key")
            .bind(server_id).fetch_all(&mut *tx).await?;
        let mut peers = Vec::with_capacity(rows.len());
        for (public_key, allocated_ip, keepalive, encrypted_psk) in rows {
            let IpAddr::V4(vpn_ip) = allocated_ip.ip() else { bail!("host agent supports IPv4 peers only") };
            let preshared_key = encrypted_psk.map(|ciphertext|
                decrypt_psk(&self.kek, &ciphertext)).transpose()?;
            peers.push(DesiredPeer {
                public_key, vpn_ip, enabled: true, preshared_key,
                persistent_keepalive: u16::try_from(keepalive).context("invalid keepalive")?,
            });
        }
        let peer_count = peers.len();
        let content_hash = DesiredState {
            revision: 0, server_public_key: server_public_key.clone(), peers: peers.clone(),
        }.digest()?;
        let status = self.request(&self.read_token, Operation::StateStatus).await?;
        let (agent_revision, agent_digest) = match status {
            Response::StateStatus { version: PROTOCOL_VERSION, applied_revision, applied_digest } =>
                (applied_revision.unwrap_or(0), applied_digest.unwrap_or_default()),
            Response::Error { code, .. } => bail!("host agent status rejected: {code}"),
            _ => bail!("unexpected host agent status response"),
        };
        let agent_revision = i64::try_from(agent_revision)?;
        if row.1 == content_hash && row.0 == agent_revision && row.2 == agent_digest {
            if row.5 != "applied" || row.3 != agent_revision || row.4 != agent_digest {
                sqlx::query("UPDATE host_agent_reconcile SET applied_revision = $2, applied_digest = $3, \
                    status = 'applied', last_error = NULL, updated_at = NOW() WHERE server_id = $1")
                    .bind(server_id).bind(agent_revision).bind(&agent_digest).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            return Ok(());
        }
        // If the journal is ahead after a crash, move beyond it. Never reuse a
        // revision or assume that an uncommitted DB status means apply failed.
        let next_revision = next_revision(row.0, agent_revision)?;
        let candidate = DesiredState {
            revision: u64::try_from(next_revision)?,
            server_public_key: server_public_key.clone(), peers,
        };
        candidate.validate(u64::try_from(agent_revision)?)?;
        let digest = candidate.digest()?;
        sqlx::query("UPDATE host_agent_reconcile SET desired_revision = $2, desired_content_hash = $3, \
                desired_digest = $4, status = 'pending', last_error = NULL, updated_at = NOW() \
                WHERE server_id = $1")
            .bind(server_id).bind(next_revision).bind(&content_hash).bind(&digest)
            .execute(&mut *tx).await?;
        let apply = self.request(&self.apply_token, Operation::ApplyState { state: candidate }).await;
        match apply {
            Ok(Response::Applied { version: PROTOCOL_VERSION, revision, digest: applied_digest })
                if revision == u64::try_from(next_revision)? && applied_digest == digest => {
                    sqlx::query("UPDATE host_agent_reconcile SET applied_revision = $2, applied_digest = $3, \
                        status = 'applied', last_error = NULL, updated_at = NOW() WHERE server_id = $1")
                        .bind(server_id).bind(next_revision).bind(&digest).execute(&mut *tx).await?;
                    tx.commit().await?;
                    info!(revision = next_revision, peer_count, "host agent state applied");
                    Ok(())
                }
            other => {
                let reason = match other {
                    Ok(Response::Error { code, .. }) => code,
                    Ok(_) => "unexpected_apply_response".to_owned(),
                    Err(error) => {
                        error!(%error, "host agent apply request failed");
                        "apply_unavailable".to_owned()
                    }
                };
                sqlx::query("UPDATE host_agent_reconcile SET status = 'failed', last_error = $2, \
                    updated_at = NOW() WHERE server_id = $1")
                    .bind(server_id).bind(&reason).execute(&mut *tx).await?;
                tx.commit().await?;
                bail!("host agent apply failed: {reason}")
            }
        }
    }
}

fn required_absolute_path(key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var(key).with_context(|| format!("{key} is required"))?);
    ensure!(path.is_absolute(), "{key} must be an absolute path");
    Ok(path)
}

fn decrypt_psk(kek: &Kek, ciphertext: &[u8]) -> Result<String> {
    let plaintext = Zeroizing::new(kek.decrypt(ciphertext)
        .context("cannot decrypt peer preshared key")?);
    Ok(std::str::from_utf8(&plaintext)
        .context("peer preshared key is not UTF-8")?.to_owned())
}

fn next_revision(database_revision: i64, agent_revision: i64) -> Result<i64> {
    ensure!(database_revision >= 0 && agent_revision >= 0, "negative host agent revision");
    database_revision.max(agent_revision).checked_add(1)
        .context("host agent revision exhausted")
}

#[async_trait]
impl WgController for HostAgentController {
    async fn add_peer(&self, _: &str, _: IpAddr, _: Option<&str>, _: u16) -> Result<(), ControlError> {
        self.reconcile().await.map_err(|error| ControlError::Other(error.to_string()))
    }

    async fn remove_peer(&self, _: &str) -> Result<(), ControlError> {
        self.reconcile().await.map_err(|error| ControlError::Other(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use zerovpn_core::models::{DeviceOs, DeviceStatus, DeviceType, UserRole, UserStatus};
    use zerovpn_db::repos::{devices, servers, users};

    #[test]
    fn revision_moves_beyond_journal_after_uncertain_commit() {
        assert_eq!(next_revision(4, 6).unwrap(), 7);
        assert_eq!(next_revision(6, 4).unwrap(), 7);
        assert!(next_revision(i64::MAX, 0).is_err());
        assert!(next_revision(-1, 0).is_err());
    }

    #[test]
    fn peer_content_fingerprint_does_not_depend_on_revision() {
        let mut state = DesiredState {
            revision: 0, server_public_key: "server".into(), peers: vec![],
        };
        let content_hash = state.digest().unwrap();
        state.revision = 8;
        assert_ne!(content_hash, state.digest().unwrap());
        state.revision = 0;
        assert_eq!(content_hash, state.digest().unwrap());
    }

    #[test]
    fn stored_peer_preshared_key_is_preserved() {
        let kek = Kek::from_b64(&STANDARD.encode([7u8; 32])).unwrap();
        let psk = STANDARD.encode([9u8; 32]);
        let encrypted = kek.encrypt(psk.as_bytes()).unwrap();
        assert_eq!(decrypt_psk(&kek, &encrypted).unwrap(), psk);
        assert!(decrypt_psk(&kek, b"bad ciphertext").is_err());
    }

    #[tokio::test]
    async fn database_and_socket_reconcile_create_pause_and_restart() -> Result<()> {
        let Ok(url) = std::env::var("ZEROVPN_TEST_DATABASE_URL") else { return Ok(()) };
        let pool = zerovpn_db::init_pool(&url, 4).await?;
        zerovpn_db::run_migrations(&pool).await?;
        let key = Arc::new(Kek::from_b64(&STANDARD.encode([7u8; 32]))?);
        let user_id = users::create(&pool, "agent-test@example.invalid", "!", UserRole::User,
            UserStatus::Active).await?;
        let server_id = servers::create(&pool, servers::NewServer {
            name: "agent-test", region: "test", endpoint_host: "vpn.test",
            endpoint_port: 51820, public_key: &STANDARD.encode([1u8; 32]),
            private_key_encrypted: b"test-encrypted-key", cidr: "10.0.0.0/22".parse()?,
            mtu: 1420,
        }).await?;
        let peer_key = STANDARD.encode([2u8; 32]);
        let psk = STANDARD.encode([3u8; 32]);
        let encrypted_psk = key.encrypt(psk.as_bytes())?;
        let device_id = devices::create(&pool, devices::NewDevice {
            user_id, server_id, name: "test device", os: DeviceOs::Other,
            device_type: DeviceType::Other, public_key: &peer_key,
            preshared_key_encrypted: Some(&encrypted_psk),
            allocated_ip: "10.0.0.2/32".parse()?,
            allowed_ips_override: None, private_key_encrypted: None,
        }).await?;

        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let latest = Arc::new(tokio::sync::Mutex::new(None::<DesiredState>));
        let latest_for_server = latest.clone();
        let fail_next_apply = Arc::new(AtomicBool::new(false));
        let fail_for_server = fail_next_apply.clone();
        let server_task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                let mut request: zerovpn_host_agent::Request = serde_json::from_str(&line).unwrap();
                let operation = std::mem::replace(&mut request.operation, Operation::Health);
                let response = match operation {
                    Operation::StateStatus => {
                        let guard = latest_for_server.lock().await;
                        Response::StateStatus { version: PROTOCOL_VERSION,
                            applied_revision: guard.as_ref().map(|state| state.revision),
                            applied_digest: guard.as_ref().map(|state| state.digest().unwrap()) }
                    }
                    Operation::ApplyState { state } => {
                        if fail_for_server.swap(false, Ordering::SeqCst) {
                            Response::Error { version: PROTOCOL_VERSION, code: "simulated_failure".into() }
                        } else {
                            let revision = state.revision;
                            let digest = state.digest().unwrap();
                            *latest_for_server.lock().await = Some(state);
                            Response::Applied { version: PROTOCOL_VERSION, revision, digest }
                        }
                    }
                    _ => panic!("unexpected operation"),
                };
                write.write_all(&serde_json::to_vec(&response).unwrap()).await.unwrap();
                write.write_all(b"\n").await.unwrap();
            }
        });
        let controller = HostAgentController { pool: pool.clone(), kek: key.clone(), socket,
            read_token: Zeroizing::new("read-token".into()),
            apply_token: Zeroizing::new("apply-token".into()) };
        controller.reconcile().await?;
        {
            let guard = latest.lock().await;
            let applied = guard.as_ref().unwrap();
            assert_eq!(applied.revision, 1);
            assert_eq!(applied.peers.len(), 1);
            assert_eq!(applied.peers[0].preshared_key.as_deref(), Some(psk.as_str()));
        }
        devices::set_status(&pool, user_id, device_id, DeviceStatus::Paused).await?;
        controller.reconcile().await?;
        assert_eq!(latest.lock().await.as_ref().unwrap().revision, 2);
        assert!(latest.lock().await.as_ref().unwrap().peers.is_empty());
        // A fresh controller reads durable DB and agent status, then skips an
        // already-applied snapshot rather than advancing the revision.
        let restarted = HostAgentController { pool: pool.clone(), kek: key.clone(),
            socket: controller.socket.clone(),
            read_token: Zeroizing::new("read-token".into()),
            apply_token: Zeroizing::new("apply-token".into()) };
        restarted.reconcile().await?;
        assert_eq!(latest.lock().await.as_ref().unwrap().revision, 2);
        devices::set_status(&pool, user_id, device_id, DeviceStatus::Active).await?;
        fail_next_apply.store(true, Ordering::SeqCst);
        assert!(controller.reconcile().await.is_err());
        let failed: (i64, i64, String) = sqlx::query_as(
            "SELECT desired_revision, applied_revision, status FROM host_agent_reconcile WHERE server_id = $1")
            .bind(server_id).fetch_one(&pool).await?;
        assert_eq!(failed, (3, 2, "failed".into()));
        controller.reconcile().await?;
        assert_eq!(latest.lock().await.as_ref().unwrap().revision, 4);
        let row: (i64, i64, String) = sqlx::query_as(
            "SELECT desired_revision, applied_revision, status FROM host_agent_reconcile WHERE server_id = $1")
            .bind(server_id).fetch_one(&pool).await?;
        assert_eq!(row, (4, 4, "applied".into()));
        server_task.abort();
        Ok(())
    }
}
