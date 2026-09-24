//! Durable identity of a *kernel interface instance*, not an agent process.

use crate::state::valid_key;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceFingerprint {
    pub boot_id: Uuid,
    pub network_namespace: String,
    pub ifindex: u32,
    pub public_key: String,
}

impl InterfaceFingerprint {
    /// The caller supplies the public key extracted from `wg show wg0 dump`.
    pub fn observe_host(public_key: &str) -> Result<Self> {
        valid_key(public_key)?;
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim().parse().context("invalid kernel boot ID")?;
        let network_namespace = fs::read_link("/proc/self/ns/net")?
            .to_string_lossy().into_owned();
        let ifindex = fs::read_to_string("/sys/class/net/wg0/ifindex")?
            .trim().parse().context("invalid wg0 ifindex")?;
        Ok(Self { boot_id, network_namespace, ifindex, public_key: public_key.to_owned() })
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.ifindex > 0, "invalid interface index");
        ensure!(self.network_namespace.starts_with("net:[") && self.network_namespace.ends_with(']'), "invalid network namespace");
        valid_key(&self.public_key)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecord {
    pub fingerprint: InterfaceFingerprint,
    pub generation: Uuid,
}

pub struct GenerationStore { path: PathBuf, lock: Mutex<()> }

impl GenerationStore {
    pub fn new(path: PathBuf) -> Self { Self { path, lock: Mutex::new(()) } }

    pub fn load(&self) -> Result<Option<GenerationRecord>> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(metadata.file_type().is_file(), "generation record must be a regular file");
        ensure!(metadata.permissions().mode() & 0o077 == 0, "generation record must be owner-only");
        let record: GenerationRecord = serde_json::from_slice(&fs::read(&self.path)?)?;
        record.fingerprint.validate()?;
        ensure!(!record.generation.is_nil(), "nil interface generation");
        Ok(Some(record))
    }

    /// Same boot/netns/ifindex/key keeps the generation through agent restarts.
    /// A changed observed instance gets a new generation before stats are sent.
    pub fn observe(&self, fingerprint: InterfaceFingerprint) -> Result<Uuid> {
        fingerprint.validate()?;
        let _guard = self.lock.lock().expect("generation mutex poisoned");
        if let Some(record) = self.load()? {
            if record.fingerprint == fingerprint { return Ok(record.generation); }
        }
        let parent = self.path.parent().context("generation path has no parent")?;
        let metadata = fs::metadata(parent).context("generation directory missing")?;
        ensure!(metadata.permissions().mode() & 0o077 == 0, "generation directory must be owner-only");
        let generation = Uuid::new_v4();
        let record = GenerationRecord { fingerprint, generation };
        let mut tmp = NamedTempFile::new_in(parent)?;
        serde_json::to_writer(tmp.as_file_mut(), &record)?;
        tmp.as_file_mut().flush()?;
        tmp.as_file_mut().sync_all()?;
        tmp.persist(&self.path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    fn fingerprint(ifindex: u32) -> InterfaceFingerprint {
        InterfaceFingerprint {
            boot_id: Uuid::from_u128(1),
            network_namespace: "net:[4026531992]".to_owned(),
            ifindex,
            public_key: STANDARD.encode([1u8; 32]),
        }
    }

    #[test]
    fn survives_process_restart_but_rotates_on_interface_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let private_dir = dir.path().join("state");
        fs::create_dir(&private_dir).unwrap();
        fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = private_dir.join("interface-generation.json");
        let first = GenerationStore::new(path.clone()).observe(fingerprint(12)).unwrap();
        let restarted = GenerationStore::new(path.clone());
        assert_eq!(first, restarted.observe(fingerprint(12)).unwrap());
        let second = restarted.observe(fingerprint(13)).unwrap();
        assert_ne!(first, second);
        assert_eq!(second, GenerationStore::new(path).load().unwrap().unwrap().generation);
    }

    #[test]
    fn corrupt_record_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interface-generation.json");
        fs::write(&path, "not JSON").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(GenerationStore::new(path).observe(fingerprint(12)).is_err());
    }
}
