//! Protected local key material. Never expose contents in errors or logs.

use crate::state::valid_key;
use anyhow::{Context, Result, ensure};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use zeroize::Zeroizing;

/// Read a canonical WireGuard private key from an owner-only regular file in
/// an owner-controlled, non-symlink directory. The final component is opened
/// with O_NOFOLLOW to avoid a check/open symlink race.
pub fn load_server_private_key(path: &Path) -> Result<Zeroizing<String>> {
    let content = load_owner_only_text(path, 128)?;
    valid_key(&content).context("invalid server private key")?;
    Ok(content)
}

pub fn load_agent_token(path: &Path) -> Result<Zeroizing<String>> {
    let content = load_owner_only_text(path, 4096)?;
    ensure!(content.len() >= 32, "agent token must contain at least 32 characters");
    ensure!(!content.chars().any(char::is_whitespace), "agent token contains whitespace");
    Ok(content)
}

fn load_owner_only_text(path: &Path, maximum_bytes: u64) -> Result<Zeroizing<String>> {
    let parent = path.parent().context("server key has no parent directory")?;
    let directory = fs::symlink_metadata(parent).context("cannot inspect server key directory")?;
    ensure!(directory.file_type().is_dir(), "server key parent must be a real directory");
    ensure!(directory.permissions().mode() & 0o022 == 0,
        "server key directory is group/world writable");
    let effective_uid = unsafe { libc::geteuid() };
    ensure!(directory.uid() == effective_uid, "server key directory owner mismatch");
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(path).context("cannot open protected server key")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "server key must be a regular file");
    ensure!(metadata.uid() == effective_uid, "server key owner mismatch");
    ensure!(metadata.permissions().mode() & 0o077 == 0, "server key must be owner-only");
    ensure!(metadata.len() <= maximum_bytes, "protected file too large");
    let mut content = Zeroizing::new(String::new());
    file.take(maximum_bytes + 1).read_to_string(&mut content)?;
    ensure!(content.len() as u64 <= maximum_bytes, "protected file too large");
    while content.ends_with('\n') || content.ends_with('\r') { content.pop(); }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn accepts_owner_only_key_and_rejects_symlink_or_open_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server.private");
        fs::write(&path, format!("{}\n", STANDARD.encode([7u8; 32]))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_server_private_key(&path).unwrap().as_str(), STANDARD.encode([7u8; 32]));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load_server_private_key(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link.private");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load_server_private_key(&link).is_err());
    }

    #[test]
    fn apply_token_needs_its_own_protected_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("apply.token");
        fs::write(&path, "abcdefghijklmnopqrstuvwxyz012345\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_agent_token(&path).unwrap().len(), 32);
        fs::write(&path, "too-short\n").unwrap();
        assert!(load_agent_token(&path).is_err());
    }
}
