//! Startup data-plane gate. No socket is opened until this returns success.

use crate::firewall_preflight::parse_iptables_save;
use crate::host_commands::{CommandHostOperations, HostCommandRunner};
use crate::host_cutover::HostCutoverBackend;
use crate::host_preflight::HostProbe;
use crate::policy::parse_ipv4_route_get;
use crate::recovery::{restore_from_journal, verify_restarted_host};
use crate::secret_file::load_server_private_key;
use crate::state::StateStore;
use crate::wireguard_config::verify_server_identity;
use anyhow::{Result, ensure};
use std::path::Path;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StartupStatus { NoAppliedState, ActiveVerified, Restored }

pub async fn prepare_startup<R: HostCommandRunner, P: HostProbe>(
    store: &StateStore, key_path: &Path, mut runner: R, mut probe: P,
) -> Result<StartupStatus> {
    let applied = store.load()?;
    let Some(applied) = applied else {
        ensure!(!probe.wg0_exists()?, "host wg0 exists without an applied journal");
        return Ok(StartupStatus::NoAppliedState);
    };
    let private_key = load_server_private_key(key_path)?;
    verify_server_identity(&private_key, &applied.state.server_public_key).await?;
    if probe.wg0_exists()? {
        verify_restarted_host(&applied, &mut probe, &mut runner)?;
        return Ok(StartupStatus::ActiveVerified);
    }
    let snapshot = probe.iptables_save()?;
    let facts = parse_iptables_save(&snapshot)?;
    let target = facts.https_target.ok_or_else(|| anyhow::anyhow!("HTTPS target missing"))?;
    let route = parse_ipv4_route_get(&probe.route_get(target)?, target)?;
    let operations = CommandHostOperations::new(runner, probe);
    let mut backend = HostCutoverBackend::new(operations, &facts, &route,
        private_key.to_string(), applied.state.server_public_key.clone()).await?;
    restore_from_journal(store, &mut backend)?;
    Ok(StartupStatus::Restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    struct NoopRunner;
    impl HostCommandRunner for NoopRunner {
        fn run(&mut self, _: &str, _: &[&str], _: Option<&[u8]>) -> Result<()> { unreachable!() }
        fn output(&mut self, _: &str, _: &[&str]) -> Result<String> { unreachable!() }
    }
    struct Probe(bool);
    impl HostProbe for Probe {
        fn iptables_save(&mut self) -> Result<String> { unreachable!() }
        fn route_get(&mut self, _: Ipv4Addr) -> Result<String> { unreachable!() }
        fn udp_listeners(&mut self) -> Result<String> { unreachable!() }
        fn wg0_exists(&mut self) -> Result<bool> { Ok(self.0) }
    }

    #[tokio::test]
    async fn empty_journal_allows_only_absent_interface_without_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        let key = dir.path().join("missing.private");
        assert_eq!(prepare_startup(&store, &key, NoopRunner, Probe(false)).await.unwrap(), StartupStatus::NoAppliedState);
        assert!(prepare_startup(&store, &key, NoopRunner, Probe(true)).await.is_err());
    }
}
