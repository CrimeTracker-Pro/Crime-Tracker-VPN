use anyhow::Context;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use zerovpn_host_agent::generation::{GenerationStore, InterfaceFingerprint};
use zerovpn_host_agent::apply_request::apply_state;
use zerovpn_host_agent::host_commands::SystemHostCommandRunner;
use zerovpn_host_agent::host_preflight::SystemHostProbe;
use zerovpn_host_agent::startup::prepare_startup;
use zerovpn_host_agent::secret_file::load_agent_token;
use zerovpn_host_agent::state::StateStore;
use zerovpn_host_agent::{MAX_MESSAGE_BYTES, Operation, PROTOCOL_VERSION, Request, Response, interface_public_key, parse_wg_dump, token_matches};
use zeroize::Zeroize;

const INTERFACE: &str = "wg0";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let socket = PathBuf::from(std::env::var("VPN_AGENT_SOCKET")
        .unwrap_or_else(|_| "/run/crimetracker-vpn/agent.sock".to_owned()));
    let token_file = PathBuf::from(std::env::var("VPN_AGENT_TOKEN_FILE")
        .unwrap_or_else(|_| "/etc/crimetracker-vpn/agent.token".to_owned()));
    let read_token = Arc::new(load_agent_token(&token_file).context("cannot load read-only agent token")?);
    let apply_token = std::env::var("VPN_AGENT_APPLY_TOKEN_FILE").ok()
        .map(|path| load_agent_token(&PathBuf::from(path))).transpose()
        .context("cannot load privileged apply token")?.map(Arc::new);
    if let Some(apply_token) = &apply_token {
        anyhow::ensure!(!token_matches(&read_token, apply_token),
            "apply token must differ from the read-only token");
    }
    let state_dir = PathBuf::from(std::env::var("VPN_AGENT_STATE_DIR")
        .unwrap_or_else(|_| "/var/lib/crimetracker-vpn/agent".to_owned()));
    let state_store = Arc::new(StateStore::new(state_dir));
    let generation_store = Arc::new(GenerationStore::new(
        state_store.directory().join("interface-generation.json")));
    generation_store.load().context("load interface generation")?;
    let server_key_file = PathBuf::from(std::env::var("VPN_AGENT_SERVER_PRIVATE_KEY_FILE")
        .unwrap_or_else(|_| "/etc/crimetracker-vpn/server.private".to_owned()));
    let startup = prepare_startup(&state_store, &server_key_file,
        SystemHostCommandRunner::default(), SystemHostProbe).await
        .context("host data-plane startup check failed")?;
    tracing::info!(?startup, "host data-plane startup check passed");

    let listener = bind_agent_socket(&socket).await.context("bind agent socket")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    tracing::info!(path = %socket.display(), apply_enabled = apply_token.is_some(), "host agent listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let read_token = Arc::clone(&read_token);
        let apply_token = apply_token.clone();
        let state_store = Arc::clone(&state_store);
        let generation_store = Arc::clone(&generation_store);
        let server_key_file = server_key_file.clone();
        tokio::spawn(async move {
            handle(stream, &read_token, apply_token.as_deref().map(|token| token.as_str()),
                &server_key_file, &state_store, &generation_store).await;
        });
    }
}

async fn bind_agent_socket(path: &PathBuf) -> anyhow::Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
        Err(error) => return Err(error.into()),
    }
    // Never unlink an active listener or an unexpected filesystem object.
    match UnixStream::connect(path).await {
        Ok(_) => anyhow::bail!("agent socket already has an active listener"),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => return Err(error.into()),
    }
    let owner = unsafe { libc::geteuid() };
    let parent = path.parent().context("socket has no parent directory")?;
    let directory = std::fs::symlink_metadata(parent)?;
    let socket = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(directory.file_type().is_dir() && directory.uid() == owner
        && directory.permissions().mode() & 0o022 == 0,
        "socket directory is not owner-controlled");
    anyhow::ensure!(socket.file_type().is_socket() && socket.uid() == owner,
        "occupied agent socket path is not an owner-held socket");
    std::fs::remove_file(path)?;
    Ok(UnixListener::bind(path)?)
}

async fn handle(stream: UnixStream, read_token: &str, apply_token: Option<&str>, server_key_file: &PathBuf,
    state_store: &StateStore, generation_store: &GenerationStore) {
    let (read, mut write) = stream.into_split();
    let mut frame = Vec::new();
    let read_result = tokio::time::timeout(Duration::from_secs(10),
        BufReader::new(read).take((MAX_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)).await;
    let response = match read_result {
        Ok(Ok(_)) if frame.len() <= MAX_MESSAGE_BYTES && frame.ends_with(b"\n") => {
            let parsed = serde_json::from_slice::<Request>(&frame);
            frame.zeroize();
            match parsed {
                Ok(request) if matches!(&request.operation, Operation::ApplyState { .. }) && apply_token.is_none() => error("apply_disabled"),
                Ok(request) if !token_matches(
                    if matches!(&request.operation, Operation::ApplyState { .. }) { apply_token.unwrap_or("") } else { read_token },
                    &request.token,
                ) => error("unauthorized"),
                Ok(request) if request.version != PROTOCOL_VERSION => error("unsupported_version"),
                Ok(mut request) => {
                    let operation = std::mem::replace(&mut request.operation, Operation::Health);
                    match operation {
                        Operation::ApplyState { state } => {
                            match apply_state(state, state_store.directory(), server_key_file).await {
                                Ok(outcome) => Response::Applied { version: PROTOCOL_VERSION,
                                    revision: outcome.revision, digest: outcome.digest },
                                Err(_) => error("apply_failed"),
                            }
                        }
                        other => tokio::time::timeout(Duration::from_secs(10),
                            execute(other, state_store, generation_store)).await
                            .unwrap_or(Err("operation_timeout"))
                            .unwrap_or_else(|code| error(code)),
                    }
                },
                Err(_) => error("invalid_request"),
            }
        }
        _ => error("invalid_frame"),
    };
    frame.zeroize();
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        if bytes.len() <= MAX_MESSAGE_BYTES {
            let _ = write.write_all(&bytes).await;
        }
    }
}

fn error(code: &str) -> Response {
    Response::Error { version: PROTOCOL_VERSION, code: code.to_owned() }
}

async fn execute(operation: Operation, state_store: &StateStore, generation_store: &GenerationStore) -> Result<Response, &'static str> {
    if matches!(operation, Operation::StateStatus) {
        let applied = state_store.load().map_err(|_| "state_unavailable")?;
        return Ok(Response::StateStatus {
            version: PROTOCOL_VERSION,
            applied_revision: applied.as_ref().map(|state| state.state.revision),
            applied_digest: applied.map(|state| state.digest),
        });
    }
    let operation = if let Operation::ValidateState { state } = operation {
        let current = state_store.load().map_err(|_| "state_unavailable")?;
        let prior_revision = current.as_ref().map_or(0, |s| s.state.revision);
        let digest = state.digest().map_err(|_| "invalid_state")?;
        let result = if state.revision <= prior_revision {
            Err("stale_revision")
        } else if current.as_ref().is_some_and(|s| s.state.server_public_key != state.server_public_key) {
            Err("server_identity_change")
        } else {
            state.validate(prior_revision).map_err(|_| "invalid_state")
        };
        let action = match &result {
            Ok(_) => "validated",
            Err("stale_revision") => "rejected_stale_revision",
            Err("server_identity_change") => "rejected_server_identity_change",
            Err(_) => "rejected_invalid_state",
        };
        state_store.audit(action, state.revision, &digest).map_err(|_| "audit_unavailable")?;
        return result.map(|digest| Response::Validated { version: PROTOCOL_VERSION, revision: state.revision, digest });
    } else { operation };
    let applied = state_store.load().map_err(|_| "state_unavailable")?;
    let before = applied.as_ref().map(|state|
        InterfaceFingerprint::observe_host(&state.state.server_public_key)
    ).transpose().map_err(|_| "generation_unavailable")?;
    let output = Command::new("wg")
        .args(["show", INTERFACE, "dump"])
        .kill_on_drop(true)
        .output().await.map_err(|_| "wg_unavailable")?;
    if !output.status.success() {
        return Err("interface_unavailable");
    }
    let dump = std::str::from_utf8(&output.stdout).map_err(|_| "invalid_wg_output")?;
    let peers = parse_wg_dump(dump).map_err(|_| "invalid_wg_output")?;
    let applied_status = applied.as_ref().map(|state| (state.state.revision, state.digest.clone()));
    let interface_generation = if let Some(applied) = applied {
        let public_key = interface_public_key(dump).map_err(|_| "invalid_wg_output")?;
        if public_key != applied.state.server_public_key {
            return Err("server_identity_mismatch");
        }
        let fingerprint = InterfaceFingerprint::observe_host(public_key)
            .map_err(|_| "generation_unavailable")?;
        if before.as_ref() != Some(&fingerprint) {
            return Err("interface_changed_during_sample");
        }
        Some(generation_store.observe(fingerprint).map_err(|_| "generation_unavailable")?)
    } else {
        None
    };
    let interface_counters = if interface_generation.is_some() {
        let base = format!("/sys/class/net/{INTERFACE}/statistics");
        let rx = std::fs::read_to_string(format!("{base}/rx_bytes"))
            .ok().and_then(|value| value.trim().parse::<u64>().ok());
        let tx = std::fs::read_to_string(format!("{base}/tx_bytes"))
            .ok().and_then(|value| value.trim().parse::<u64>().ok());
        rx.zip(tx)
    } else { None };
    if interface_generation.is_some() {
        let public_key = interface_public_key(dump).map_err(|_| "invalid_wg_output")?;
        let after = InterfaceFingerprint::observe_host(public_key)
            .map_err(|_| "generation_unavailable")?;
        if before.as_ref() != Some(&after) {
            return Err("interface_changed_during_sample");
        }
    }
    Ok(match operation {
        Operation::Health => Response::Health {
            version: PROTOCOL_VERSION, interface: INTERFACE.to_owned(), interface_generation, peer_count: peers.len(),
            applied_revision: applied_status.as_ref().map(|(revision, _)| *revision),
            applied_digest: applied_status.map(|(_, digest)| digest),
        },
        Operation::PeerStats => Response::PeerStats {
            version: PROTOCOL_VERSION, interface: INTERFACE.to_owned(), interface_generation,
            interface_rx_bytes: interface_counters.map(|counters| counters.0),
            interface_tx_bytes: interface_counters.map(|counters| counters.1), peers,
        },
        Operation::ValidateState { .. } => unreachable!(),
        Operation::ApplyState { .. } => unreachable!(),
        Operation::StateStatus => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use zerovpn_host_agent::state::{DesiredPeer, DesiredState};

    fn state(revision: u64) -> DesiredState {
        DesiredState {
            revision,
            server_public_key: STANDARD.encode([1u8; 32]),
            peers: vec![DesiredPeer {
                public_key: STANDARD.encode([2u8; 32]),
                vpn_ip: "10.0.0.2".parse().unwrap(),
                enabled: true,
                preshared_key: None,
                persistent_keepalive: 30,
            }],
        }
    }

    #[tokio::test]
    async fn validation_audits_without_applying() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        let generation = GenerationStore::new(dir.path().join("generation.json"));
        let response = execute(Operation::ValidateState { state: state(1) }, &store, &generation).await.unwrap();
        assert!(matches!(response, Response::Validated { revision: 1, .. }));
        assert!(store.load().unwrap().is_none());
        store.record_applied(state(1)).unwrap();
        assert_eq!(execute(Operation::ValidateState { state: state(1) }, &store, &generation).await.unwrap_err(), "stale_revision");
        let mut changed_identity = state(2);
        changed_identity.server_public_key = STANDARD.encode([3u8; 32]);
        assert_eq!(execute(Operation::ValidateState { state: changed_identity }, &store, &generation).await.unwrap_err(), "server_identity_change");
        let audit = std::fs::read_to_string(store.directory().join("audit.jsonl")).unwrap();
        assert!(audit.contains("validated"));
        assert!(audit.contains("rejected_stale_revision"));
        assert!(audit.contains("rejected_server_identity_change"));
        assert!(!audit.contains("10.0.0.2"));
    }

    #[tokio::test]
    async fn journal_status_works_without_wg_interface() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        let generation = GenerationStore::new(dir.path().join("generation.json"));
        assert!(matches!(execute(Operation::StateStatus, &store, &generation).await.unwrap(),
            Response::StateStatus { applied_revision: None, applied_digest: None, .. }));
        let applied = store.record_applied(state(1)).unwrap();
        assert!(matches!(execute(Operation::StateStatus, &store, &generation).await.unwrap(),
            Response::StateStatus { applied_revision: Some(1), applied_digest: Some(digest), .. }
                if digest == applied.digest));
    }

    #[tokio::test]
    async fn stale_socket_is_recovered_but_live_listener_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.sock");
        let live = bind_agent_socket(&path).await.unwrap();
        assert!(bind_agent_socket(&path).await.is_err());
        drop(live);
        let recovered = bind_agent_socket(&path).await.unwrap();
        drop(recovered);
    }

    #[tokio::test]
    async fn apply_requires_separate_token_and_never_falls_back_to_read_token() {
        async fn send(token: &str, apply_token: Option<&str>, store: &StateStore,
            generation: &GenerationStore, key: &PathBuf) -> Response {
            let (mut client, server) = UnixStream::pair().unwrap();
            let request = Request { version: PROTOCOL_VERSION, token: token.to_owned(),
                operation: Operation::ApplyState { state: state(1) } };
            let mut payload = serde_json::to_vec(&request).unwrap();
            payload.push(b'\n');
            let exchange = async {
                client.write_all(&payload).await.unwrap();
                let mut frame = Vec::new();
                BufReader::new(client).read_until(b'\n', &mut frame).await.unwrap();
                serde_json::from_slice::<Response>(&frame).unwrap()
            };
            let (_, response) = tokio::join!(
                handle(server, "read-token", apply_token, key, store, generation), exchange
            );
            response
        }

        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        let generation = GenerationStore::new(dir.path().join("generation.json"));
        let key = dir.path().join("missing.private");
        assert!(matches!(send("read-token", None, &store, &generation, &key).await,
            Response::Error { code, .. } if code == "apply_disabled"));
        assert!(matches!(send("read-token", Some("apply-token"), &store, &generation, &key).await,
            Response::Error { code, .. } if code == "unauthorized"));
        assert!(matches!(send("apply-token", Some("apply-token"), &store, &generation, &key).await,
            Response::Error { code, .. } if code == "apply_failed"));
        assert!(store.load().unwrap().is_none());
    }
}
