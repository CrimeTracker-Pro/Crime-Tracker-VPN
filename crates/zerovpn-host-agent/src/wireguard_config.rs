//! Offline renderer for the server-side `wg` configuration format.
//! No interface creation, shell invocation, or host mutation occurs here.

use crate::state::{DesiredState, valid_key};
use anyhow::{Result, ensure};
use std::fmt;
use std::fmt::Write as _;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zeroize::Zeroize;

pub const LISTEN_PORT: u16 = 51820;

/// Verify identity through `wg pubkey` without putting the private key in
/// argv, logs, or an on-disk temporary file. This never mutates an interface.
pub async fn verify_server_identity(server_private_key: &str, expected_public_key: &str) -> Result<()> {
    valid_key(server_private_key)?;
    valid_key(expected_public_key)?;
    let mut child = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("wg stdin unavailable"))?;
    stdin.write_all(server_private_key.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await??;
    ensure!(output.status.success(), "wg pubkey failed");
    let actual = std::str::from_utf8(&output.stdout)?.trim();
    ensure!(actual == expected_public_key, "server private/public key mismatch");
    Ok(())
}

/// May contain the server private key and peer PSKs. Never derive Display or
/// expose the full value in logs, audit records, or API responses.
pub struct SensitiveWgConfig(String);

impl SensitiveWgConfig {
    pub fn as_bytes(&self) -> &[u8] { self.0.as_bytes() }
}

impl fmt::Debug for SensitiveWgConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveWgConfig(<redacted>)")
    }
}

impl Drop for SensitiveWgConfig {
    fn drop(&mut self) { self.0.zeroize(); }
}

/// The caller must independently prove that `server_private_key` derives to
/// `state.server_public_key` before using the rendered configuration.
pub fn render(state: &DesiredState, server_private_key: &str) -> Result<SensitiveWgConfig> {
    state.validate(state.revision.saturating_sub(1))?;
    valid_key(server_private_key)?;
    ensure!(!state.server_public_key.is_empty(), "missing server identity");
    let mut peers: Vec<_> = state.peers.iter().filter(|peer| peer.enabled).collect();
    peers.sort_by(|a, b| a.public_key.cmp(&b.public_key));
    let mut config = String::new();
    writeln!(&mut config, "[Interface]")?;
    writeln!(&mut config, "PrivateKey = {server_private_key}")?;
    writeln!(&mut config, "ListenPort = {LISTEN_PORT}")?;
    for peer in peers {
        config.push_str("\n[Peer]\n");
        writeln!(&mut config, "PublicKey = {}", peer.public_key)?;
        if let Some(psk) = &peer.preshared_key {
            writeln!(&mut config, "PresharedKey = {psk}")?;
        }
        writeln!(&mut config, "AllowedIPs = {}/32", peer.vpn_ip)?;
        writeln!(&mut config, "PersistentKeepalive = {}", peer.persistent_keepalive)?;
    }
    Ok(SensitiveWgConfig(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DesiredPeer;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    fn key(byte: u8) -> String { STANDARD.encode([byte; 32]) }

    #[test]
    fn renders_only_enabled_peers_with_keepalive_and_psk() {
        let state = DesiredState {
            revision: 1,
            server_public_key: key(1),
            peers: vec![
                DesiredPeer { public_key: key(3), vpn_ip: "10.0.3.254".parse().unwrap(), enabled: true, preshared_key: Some(key(4)), persistent_keepalive: 30 },
                DesiredPeer { public_key: key(2), vpn_ip: "10.0.0.2".parse().unwrap(), enabled: false, preshared_key: None, persistent_keepalive: 30 },
            ],
        };
        let config = render(&state, &key(5)).unwrap();
        let output = std::str::from_utf8(config.as_bytes()).unwrap();
        assert!(output.contains("ListenPort = 51820"));
        assert!(output.contains("AllowedIPs = 10.0.3.254/32"));
        assert!(output.contains("PersistentKeepalive = 30"));
        assert!(output.contains(&format!("PresharedKey = {}", key(4))));
        assert!(!output.contains("10.0.0.2/32"));
        assert!(!format!("{config:?}").contains(&key(4)));
        assert!(!format!("{config:?}").contains(&key(5)));
    }

    #[test]
    fn bad_preshared_key_is_rejected() {
        let state = DesiredState {
            revision: 1,
            server_public_key: key(1),
            peers: vec![DesiredPeer { public_key: key(2), vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true, preshared_key: Some("bad".into()), persistent_keepalive: 30 }],
        };
        assert!(render(&state, &key(3)).is_err());
    }

    #[tokio::test]
    async fn invalid_private_key_fails_before_command_execution() {
        assert!(verify_server_identity("bad", &key(1)).await.is_err());
    }
}
