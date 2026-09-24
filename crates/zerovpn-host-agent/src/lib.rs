//! Versioned host-agent protocol. No WireGuard private keys cross this API.

pub mod state;
pub mod apply;
pub mod generation;
pub mod policy;
pub mod wireguard_config;
pub mod firewall_preflight;
pub mod firewall_executor;
pub mod host_cutover;
pub mod host_preflight;
pub mod host_commands;
pub mod recovery;
pub mod peer_update;
pub mod secret_file;
pub mod startup;
pub mod apply_request;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use zeroize::Zeroize;

pub const PROTOCOL_VERSION: u16 = 2;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub token: String,
    pub operation: Operation,
}

impl Drop for Request {
    fn drop(&mut self) { self.token.zeroize(); }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum Operation {
    Health,
    /// Reads only the durable journal; works before the first host interface exists.
    StateStatus,
    PeerStats,
    /// Checks a complete candidate; does not apply it or advance the revision.
    ValidateState { state: state::DesiredState },
    /// Requires a separate privileged token; never accepted with the stats token.
    ApplyState { state: state::DesiredState },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Health {
        version: u16,
        interface: String,
        interface_generation: Option<uuid::Uuid>,
        peer_count: usize,
        /// The durable host journal revision, if a state has been applied.
        #[serde(default)] applied_revision: Option<u64>,
        #[serde(default)] applied_digest: Option<String>,
    },
    StateStatus { version: u16, applied_revision: Option<u64>, applied_digest: Option<String> },
    PeerStats {
        version: u16,
        interface: String,
        interface_generation: Option<uuid::Uuid>,
        #[serde(default)] interface_rx_bytes: Option<u64>,
        #[serde(default)] interface_tx_bytes: Option<u64>,
        peers: Vec<PeerStats>,
    },
    Validated { version: u16, revision: u64, digest: String },
    Applied { version: u16, revision: u64, digest: String },
    Error { version: u16, code: String },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerStats {
    pub public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: String,
    pub latest_handshake: i64,
    /// Bytes received by the server from this peer.
    pub rx_bytes: u64,
    /// Bytes sent by the server to this peer.
    pub tx_bytes: u64,
}

/// Parse `wg show wg0 dump`; never retain the interface private key or peer PSKs.
pub fn parse_wg_dump(dump: &str) -> anyhow::Result<Vec<PeerStats>> {
    let mut lines = dump.lines();
    let header = lines.next().ok_or_else(|| anyhow::anyhow!("missing interface row"))?;
    anyhow::ensure!(header.split('\t').count() == 4, "invalid interface row");
    let mut peers = Vec::new();
    for line in lines {
        let cols: Vec<_> = line.split('\t').collect();
        anyhow::ensure!(cols.len() == 8, "invalid peer row");
        anyhow::ensure!(!cols[0].is_empty(), "missing public key");
        peers.push(PeerStats {
            public_key: cols[0].to_owned(),
            endpoint: (cols[2] != "(none)" && !cols[2].is_empty()).then(|| cols[2].to_owned()),
            allowed_ips: cols[3].to_owned(),
            latest_handshake: cols[4].parse()?,
            rx_bytes: cols[5].parse()?,
            tx_bytes: cols[6].parse()?,
        });
    }
    Ok(peers)
}

/// Read only the public key from the secret-bearing `wg show ... dump` header.
pub fn interface_public_key(dump: &str) -> anyhow::Result<&str> {
    let header = dump.lines().next().ok_or_else(|| anyhow::anyhow!("missing interface row"))?;
    let cols: Vec<_> = header.split('\t').collect();
    anyhow::ensure!(cols.len() == 4, "invalid interface row");
    state::valid_key(cols[1])?;
    Ok(cols[1])
}

/// Compare token hashes without a data-dependent early return.
pub fn token_matches(expected: &str, supplied: &str) -> bool {
    let a = Sha256::digest(expected.as_bytes());
    let b = Sha256::digest(supplied.as_bytes());
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn query(socket: &Path, token: &str, operation: Operation) -> anyhow::Result<Response> {
    let mut stream = UnixStream::connect(socket).await?;
    let request = Request { version: PROTOCOL_VERSION, token: token.to_owned(), operation };
    let mut payload = serde_json::to_vec(&request)?;
    if payload.len() > MAX_MESSAGE_BYTES {
        payload.zeroize();
        anyhow::bail!("request too large");
    }
    payload.push(b'\n');
    let write_result = stream.write_all(&payload).await;
    payload.zeroize();
    write_result?;
    let reader = BufReader::new(stream);
    let mut response = Vec::new();
    reader.take((MAX_MESSAGE_BYTES + 1) as u64).read_until(b'\n', &mut response).await?;
    anyhow::ensure!(response.len() <= MAX_MESSAGE_BYTES && response.ends_with(b"\n"), "invalid response frame");
    Ok(serde_json::from_slice(&response)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_excludes_private_and_preshared_keys() {
        let dump = "private-secret\tpublic\t51820\toff\npeer-public\tpsk-secret\t(none)\t10.0.0.2/32\t0\t120\t40\toff\n";
        let peers = parse_wg_dump(dump).unwrap();
        assert_eq!(peers[0].rx_bytes, 120);
        let serialized = serde_json::to_string(&peers).unwrap();
        assert!(!serialized.contains("private-secret"));
        assert!(!serialized.contains("psk-secret"));
    }

    #[test]
    fn malformed_dump_is_rejected() {
        assert!(parse_wg_dump("private\tpublic\t51820\toff\npeer\tpsk\n").is_err());
        assert!(parse_wg_dump("private\tpublic\t51820\toff\npeer\tpsk\t(none)\t10.0.0.2/32\t0\tbad\t0\toff\n").is_err());
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches("a-secret", "a-secret"));
        assert!(!token_matches("a-secret", "other"));
    }

    #[test]
    fn health_status_is_backward_compatible() {
        let old = r#"{"type":"health","version":2,"interface":"wg0","interface_generation":null,"peer_count":0}"#;
        let response: Response = serde_json::from_str(old).unwrap();
        assert!(matches!(response, Response::Health {
            applied_revision: None, applied_digest: None, ..
        }));
        let current = Response::Health {
            version: PROTOCOL_VERSION,
            interface: "wg0".into(),
            interface_generation: None,
            peer_count: 0,
            applied_revision: Some(7),
            applied_digest: Some("digest".into()),
        };
        let encoded = serde_json::to_value(current).unwrap();
        assert_eq!(encoded["applied_revision"], 7);
        assert_eq!(encoded["applied_digest"], "digest");
    }
}
