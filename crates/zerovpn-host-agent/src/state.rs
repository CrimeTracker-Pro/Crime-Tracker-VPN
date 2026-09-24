//! Whole-state validation and durable applied-state journal. No host mutation here.

use anyhow::Context;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::NamedTempFile;
use zeroize::Zeroize;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredState {
    pub revision: u64,
    pub server_public_key: String,
    pub peers: Vec<DesiredPeer>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredPeer {
    pub public_key: String,
    pub vpn_ip: Ipv4Addr,
    pub enabled: bool,
    /// Optional WireGuard preshared key; secret, never include in logs.
    #[serde(default)]
    pub preshared_key: Option<String>,
    #[serde(default)]
    pub persistent_keepalive: u16,
}

impl Drop for DesiredPeer {
    fn drop(&mut self) {
        if let Some(psk) = &mut self.preshared_key { psk.zeroize(); }
    }
}

impl DesiredState {
    pub fn digest(&self) -> anyhow::Result<String> {
        let mut bytes = serde_json::to_vec(self)?;
        let digest = Sha256::digest(&bytes);
        bytes.zeroize();
        Ok(format!("{digest:x}"))
    }

    pub fn validate(&self, applied_revision: u64) -> anyhow::Result<String> {
        anyhow::ensure!(self.revision > applied_revision, "stale_revision");
        valid_key(&self.server_public_key).context("invalid server public key")?;
        anyhow::ensure!(self.peers.len() <= 4096, "too_many_peers");
        let mut keys = HashSet::new();
        let mut ips = HashSet::new();
        for peer in &self.peers {
            valid_key(&peer.public_key).context("invalid peer public key")?;
            if let Some(psk) = &peer.preshared_key {
                valid_key(psk).context("invalid peer preshared key")?;
            }
            anyhow::ensure!(peer.public_key != self.server_public_key, "server key used as peer");
            anyhow::ensure!(keys.insert(&peer.public_key), "duplicate peer key");
            let octets = peer.vpn_ip.octets();
            // The deployed server is 10.0.0.1/22. Keep the full /22 usable,
            // except its network/broadcast and the gateway address.
            let in_subnet = octets[0] == 10 && octets[1] == 0 && octets[2] <= 3;
            let reserved = (octets[2] == 0 && octets[3] <= 1)
                || (octets[2] == 3 && octets[3] == 255);
            anyhow::ensure!(in_subnet && !reserved, "peer IP outside usable 10.0.0.0/22");
            anyhow::ensure!(ips.insert(peer.vpn_ip), "duplicate peer IP");
        }
        self.digest()
    }
}

pub(crate) fn valid_key(key: &str) -> anyhow::Result<()> {
    let bytes = STANDARD.decode(key)?;
    anyhow::ensure!(bytes.len() == 32 && STANDARD.encode(bytes) == key, "invalid WireGuard key");
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedState {
    pub state: DesiredState,
    pub digest: String,
}

pub struct StateStore { directory: PathBuf, lock: Mutex<()> }

impl StateStore {
    pub fn new(directory: PathBuf) -> Self { Self { directory, lock: Mutex::new(()) } }

    pub fn load(&self) -> anyhow::Result<Option<AppliedState>> {
        let path = self.directory.join("last-known-good.json");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(metadata.file_type().is_file(), "last-known-good must be a regular file");
        anyhow::ensure!(metadata.permissions().mode() & 0o077 == 0, "last-known-good must be owner-only");
        let mut bytes = fs::read(path)?;
        let parsed = serde_json::from_slice(&bytes);
        bytes.zeroize();
        let applied: AppliedState = parsed?;
        let digest = applied.state.validate(applied.state.revision.saturating_sub(1))?;
        anyhow::ensure!(digest == applied.digest, "last-known-good digest mismatch");
        Ok(Some(applied))
    }

    /// Record a state only after the external host apply has succeeded and verified.
    /// This method deliberately performs no `wg` or firewall operation.
    pub fn record_applied(&self, candidate: DesiredState) -> anyhow::Result<AppliedState> {
        let _guard = self.lock.lock().expect("state mutex poisoned");
        let previous = self.load()?;
        let prior_revision = previous.as_ref().map_or(0, |s| s.state.revision);
        let digest = candidate.validate(prior_revision)?;
        self.ensure_directory()?;
        let applied = AppliedState { state: candidate, digest };
        let mut bytes = serde_json::to_vec(&applied)?;
        let mut tmp = NamedTempFile::new_in(&self.directory)?;
        let write_result = tmp.as_file_mut().write_all(&bytes);
        bytes.zeroize();
        write_result?;
        tmp.as_file_mut().sync_all()?;
        tmp.persist(self.directory.join("last-known-good.json"))?;
        fs::File::open(&self.directory)?.sync_all()?;
        Ok(applied)
    }

    pub fn audit(&self, action: &str, revision: u64, digest: &str) -> anyhow::Result<()> {
        let _guard = self.lock.lock().expect("state mutex poisoned");
        self.audit_entry(action, revision, digest)
    }

    fn audit_entry(&self, action: &str, revision: u64, digest: &str) -> anyhow::Result<()> {
        self.ensure_directory()?;
        let path = self.directory.join("audit.jsonl");
        let mut file = OpenOptions::new().append(true).create(true).mode(0o600).open(path)?;
        let entry = serde_json::json!({ "action": action, "revision": revision, "digest": digest });
        let mut encoded = serde_json::to_vec(&entry)?;
        encoded.push(b'\n');
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(())
    }

    pub fn directory(&self) -> &Path { &self.directory }

    /// Advisory cross-process guard held for the whole apply transaction.
    /// The protected directory prevents untrusted users replacing the lock.
    pub fn acquire_apply_lock(&self) -> anyhow::Result<fs::File> {
        self.ensure_directory()?;
        let file = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
            .open(self.directory.join("apply.lock"))?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
            "apply lock must be an owner-only regular file");
        file.try_lock().context("another host apply is already running")?;
        Ok(file)
    }

    fn ensure_directory(&self) -> anyhow::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.directory)?;
        let metadata = fs::metadata(&self.directory)?;
        anyhow::ensure!(metadata.permissions().mode() & 0o077 == 0, "state directory must be owner-only");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> String { STANDARD.encode([byte; 32]) }
    fn state(revision: u64) -> DesiredState {
        DesiredState {
            revision,
            server_public_key: key(1),
            peers: vec![DesiredPeer { public_key: key(2), vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true, preshared_key: None, persistent_keepalive: 30 }],
        }
    }

    #[test]
    fn validates_complete_state_and_rejects_stale_or_unsafe() {
        assert!(state(2).validate(1).is_ok());
        let mut far_peer = state(2);
        far_peer.peers[0].vpn_ip = "10.0.3.254".parse().unwrap();
        assert!(far_peer.validate(1).is_ok());
        assert!(state(1).validate(1).is_err());
        let mut invalid = state(2);
        invalid.peers[0].vpn_ip = "10.0.0.1".parse().unwrap();
        assert!(invalid.validate(1).is_err());
        invalid.peers[0].vpn_ip = "10.0.3.255".parse().unwrap();
        assert!(invalid.validate(1).is_err());
    }

    #[test]
    fn old_host_access_allowlists_are_not_part_of_v2_state() {
        let mut value = serde_json::to_value(state(2)).unwrap();
        value.as_object_mut().unwrap().insert("host_access".into(), serde_json::json!([]));
        assert!(serde_json::from_value::<DesiredState>(value).is_err());
    }

    #[test]
    fn durable_state_rejects_replay_and_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        store.record_applied(state(1)).unwrap();
        assert_eq!(store.load().unwrap().unwrap().state.revision, 1);
        assert!(store.record_applied(state(1)).is_err());
        let file = store.directory().join("last-known-good.json");
        let mut bytes = fs::read_to_string(&file).unwrap();
        bytes = bytes.replace("10.0.0.2", "10.0.0.3");
        fs::write(file, bytes).unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn apply_lock_excludes_other_process_handles_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = StateStore::new(dir.path().join("state"));
        let second = StateStore::new(dir.path().join("state"));
        let guard = first.acquire_apply_lock().unwrap();
        assert!(second.acquire_apply_lock().is_err());
        drop(guard);
        second.acquire_apply_lock().unwrap();
    }
}
