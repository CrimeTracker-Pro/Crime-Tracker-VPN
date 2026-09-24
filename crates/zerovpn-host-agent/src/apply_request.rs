//! Privileged apply orchestration. The socket layer enforces a separate token.

use crate::apply::ApplyCoordinator;
use crate::firewall_preflight::parse_iptables_save;
use crate::host_commands::{CommandHostOperations, SystemHostCommandRunner};
use crate::host_cutover::HostCutoverBackend;
use crate::host_preflight::{HostProbe, SystemHostProbe};
use crate::peer_update::PeerUpdateBackend;
use crate::policy::parse_ipv4_route_get;
use crate::secret_file::load_server_private_key;
use crate::state::{DesiredState, StateStore};
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

pub struct AppliedReply { pub revision: u64, pub digest: String }

/// Chooses first cutover or existing-interface update. Every mutation runs
/// behind ApplyCoordinator's cross-process lock and rollback journal.
pub async fn apply_state(state: DesiredState, state_directory: &Path, key_path: &Path) -> Result<AppliedReply> {
    let directory = PathBuf::from(state_directory);
    let previous = StateStore::new(directory.clone()).load()?;
    let private_key = load_server_private_key(key_path)?;
    let mut probe = SystemHostProbe;
    if let Some(previous) = previous {
        ensure!(probe.wg0_exists()?, "host wg0 missing; restart recovery required");
        let backend = PeerUpdateBackend::new(SystemHostCommandRunner::default(), probe,
            previous, private_key.to_string()).await?;
        let outcome = tokio::task::spawn_blocking(move ||
            ApplyCoordinator::new(backend, StateStore::new(directory)).apply(state)
        ).await??;
        return Ok(AppliedReply { revision: outcome.applied.state.revision, digest: outcome.applied.digest });
    }
    ensure!(!probe.wg0_exists()?, "host wg0 exists without applied journal");
    let snapshot = probe.iptables_save()?;
    let facts = parse_iptables_save(&snapshot)?;
    let target = facts.https_target.ok_or_else(|| anyhow::anyhow!("HTTPS target missing"))?;
    let route = parse_ipv4_route_get(&probe.route_get(target)?, target)?;
    let operations = CommandHostOperations::new(SystemHostCommandRunner::default(), probe);
    let backend = HostCutoverBackend::new(operations, &facts, &route,
        private_key.to_string(), state.server_public_key.clone()).await?;
    let outcome = tokio::task::spawn_blocking(move ||
        ApplyCoordinator::new(backend, StateStore::new(directory)).apply(state)
    ).await??;
    Ok(AppliedReply { revision: outcome.applied.state.revision, digest: outcome.applied.digest })
}
