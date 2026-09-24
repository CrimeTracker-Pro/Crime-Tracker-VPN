//! Explicit WireGuard stats source. Host mode never falls back to container wg.

use anyhow::{Context, Result, ensure};
use std::path::PathBuf;
use std::time::Duration;
use zeroize::Zeroizing;
use zerovpn_host_agent::{Operation, PeerStats, PROTOCOL_VERSION, Response, parse_wg_dump, query};
use zerovpn_host_agent::secret_file::load_agent_token;

pub struct StatsSnapshot {
    pub source_generation: String,
    pub peers: Vec<PeerStats>,
    pub interface_counters: Option<(u64, u64)>,
}

#[derive(Clone)]
pub enum StatsSource {
    Legacy { interface: String },
    HostAgent { socket: PathBuf, token: Zeroizing<String> },
}

impl StatsSource {
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var("ZEROVPN_WORKER__WG_STATS_SOURCE") {
            Ok(source) if source == "host_agent" => {
                let socket = PathBuf::from(std::env::var("ZEROVPN_WORKER__HOST_AGENT_SOCKET")
                    .context("host-agent socket path is required")?);
                let token_path = PathBuf::from(std::env::var("ZEROVPN_WORKER__HOST_AGENT_TOKEN_FILE")
                    .context("host-agent read token file is required")?);
                ensure!(socket.is_absolute() && token_path.is_absolute(),
                    "host-agent paths must be absolute");
                let token = load_agent_token(&token_path)
                    .context("cannot read protected host-agent token file")?;
                Ok(Some(Self::HostAgent { socket, token }))
            }
            Ok(source) if source == "legacy" => Ok(Self::legacy_source()),
            Err(std::env::VarError::NotPresent) => Ok(Self::legacy_source()),
            Ok(other) => anyhow::bail!("unsupported WG stats source: {other}"),
            Err(error) => Err(error.into()),
        }
    }

    fn legacy_source() -> Option<Self> {
        let enabled = std::env::var("ZEROVPN_WG__BACKEND")
            .is_ok_and(|value| value == "shell" || value == "kernel");
        enabled.then(|| Self::Legacy {
            interface: std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into()),
        })
    }

    pub fn name(&self) -> &'static str {
        match self { Self::Legacy { .. } => "legacy", Self::HostAgent { .. } => "host_agent" }
    }

    pub async fn snapshot(&mut self) -> Result<StatsSnapshot> {
        match self {
            Self::Legacy { interface } => {
                let output = tokio::time::timeout(Duration::from_secs(5),
                    tokio::process::Command::new("wg").args(["show", interface, "dump"])
                        .kill_on_drop(true).output()).await??;
                let stdout = Zeroizing::new(output.stdout);
                ensure!(output.status.success(), "legacy wg observation failed");
                let peers = parse_wg_dump(std::str::from_utf8(&stdout)?)?;
                Ok(StatsSnapshot { source_generation: "legacy:wg0".into(), peers, interface_counters: None })
            }
            Self::HostAgent { socket, token } => {
                let response = tokio::time::timeout(Duration::from_secs(5),
                    query(socket, token, Operation::PeerStats)).await??;
                snapshot_from_agent(response)
            }
        }
    }
}

fn snapshot_from_agent(response: Response) -> Result<StatsSnapshot> {
    match response {
        Response::PeerStats { version, interface, interface_generation: Some(generation),
            interface_rx_bytes, interface_tx_bytes, peers }
            if version == PROTOCOL_VERSION && interface == "wg0" => {
                Ok(StatsSnapshot { source_generation: format!("host:{generation}"), peers,
                    interface_counters: interface_rx_bytes.zip(interface_tx_bytes) })
            }
        Response::Error { code, .. } => anyhow::bail!("host-agent stats rejected: {code}"),
        _ => anyhow::bail!("host-agent stats missing verified interface generation"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn host_generation_is_required_and_stable() {
        let generation = Uuid::new_v4();
        let response = Response::PeerStats { version: PROTOCOL_VERSION, interface: "wg0".into(),
            interface_generation: Some(generation), interface_rx_bytes: Some(100),
            interface_tx_bytes: Some(200), peers: vec![] };
        assert_eq!(snapshot_from_agent(response).unwrap().source_generation,
            format!("host:{generation}"));
        let missing = Response::PeerStats { version: PROTOCOL_VERSION, interface: "wg0".into(),
            interface_generation: None, interface_rx_bytes: None, interface_tx_bytes: None, peers: vec![] };
        assert!(snapshot_from_agent(missing).is_err());
    }

    #[tokio::test]
    async fn host_source_uses_authenticated_socket_without_legacy_fallback() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let generation = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut frame = Vec::new();
            BufReader::new(read).read_until(b'\n', &mut frame).await.unwrap();
            let request: zerovpn_host_agent::Request = serde_json::from_slice(&frame).unwrap();
            assert_eq!(request.token, "a-32-character-host-read-token-0001");
            assert!(matches!(request.operation, Operation::PeerStats));
            let reply = Response::PeerStats { version: PROTOCOL_VERSION, interface: "wg0".into(),
                interface_generation: Some(generation), interface_rx_bytes: Some(1000),
                interface_tx_bytes: Some(500), peers: vec![PeerStats {
                    public_key: "peer".into(), endpoint: None, allowed_ips: "10.0.0.2/32".into(),
                    latest_handshake: 10, rx_bytes: 120, tx_bytes: 40,
                }] };
            let mut bytes = serde_json::to_vec(&reply).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).await.unwrap();
        });
        let mut source = StatsSource::HostAgent { socket,
            token: Zeroizing::new("a-32-character-host-read-token-0001".into()) };
        let snapshot = source.snapshot().await.unwrap();
        assert_eq!(snapshot.source_generation, format!("host:{generation}"));
        assert_eq!((snapshot.peers[0].rx_bytes, snapshot.peers[0].tx_bytes), (120, 40));
        assert_eq!(snapshot.interface_counters, Some((1000, 500)));
        server.await.unwrap();
    }
}
