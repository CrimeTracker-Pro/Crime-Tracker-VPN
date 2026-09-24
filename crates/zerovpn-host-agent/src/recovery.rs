//! Read-only reconciliation after an agent process restart. No adoption or mutation.

use crate::firewall_executor::{FirewallCommand, FirewallCommands, FirewallRunner};
use crate::apply::HostBackend;
use crate::firewall_preflight::parse_iptables_save;
use crate::host_commands::{HostCommandRunner, verify_wireguard_state};
use crate::host_preflight::HostProbe;
use crate::policy::{compile_host_forwarding, parse_ipv4_route_get};
use crate::state::{AppliedState, StateStore};
use anyhow::{Context, Result, anyhow, ensure};

struct FirewallCheck<'a, R>(&'a mut R);
impl<R: HostCommandRunner> FirewallRunner for FirewallCheck<'_, R> {
    fn run(&mut self, command: &FirewallCommand) -> Result<()> {
        let args: Vec<_> = command.args.iter().map(String::as_str).collect();
        self.0.run("iptables", &args, None)
    }
}

/// Returns success only when the protected journal, live WireGuard interface,
/// current Docker route, and every VPN-owned firewall rule agree. A mismatch
/// requires operator recovery; this function never repairs or deletes state.
pub fn verify_restarted_host(
    applied: &AppliedState, probe: &mut impl HostProbe, runner: &mut impl HostCommandRunner,
) -> Result<()> {
    let digest = applied.state.validate(applied.state.revision.saturating_sub(1))?;
    ensure!(digest == applied.digest, "applied journal digest mismatch");
    ensure!(probe.wg0_exists()?, "host wg0 missing after restart");
    let snapshot = probe.iptables_save()?;
    let facts = parse_iptables_save(&snapshot)?;
    let target = facts.https_target.ok_or_else(|| anyhow::anyhow!("HTTPS target missing"))?;
    let route = parse_ipv4_route_get(&probe.route_get(target)?, target)?;
    let plan = compile_host_forwarding(&facts, &route)?;
    verify_wireguard_state(runner, &applied.state)?;
    FirewallCommands::from_plan(&plan)?.verify_installed(&mut FirewallCheck(runner), &snapshot)?;
    Ok(())
}

/// Restore a missing host tunnel from the protected journal without advancing
/// its revision. The backend's snapshot must reject an existing/unknown wg0
/// or owned firewall state. A private key is supplied to the backend through
/// a separate protected source, never read from the journal.
pub fn restore_from_journal<B: HostBackend>(store: &StateStore, backend: &mut B) -> Result<AppliedState> {
    let _process_lock = store.acquire_apply_lock().context("acquire host restore lock")?;
    let applied = store.load()?.context("no last-known-good state to restore")?;
    store.audit("restore_started", applied.state.revision, &applied.digest)?;
    let snapshot = backend.snapshot().context("host is not ready for journal restore")?;
    let result = backend.stage(&applied.state)
        .and_then(|_| backend.activate())
        .and_then(|_| backend.verify(&applied.state));
    if let Err(error) = result {
        match backend.rollback(snapshot) {
            Ok(()) => {
                store.audit("restore_rolled_back", applied.state.revision, &applied.digest)?;
                return Err(error.context("journal restore failed; host state rolled back"));
            }
            Err(rollback_error) => {
                let _ = store.audit("restore_rollback_failed", applied.state.revision, &applied.digest);
                return Err(anyhow!("journal restore failed: {error}; rollback failed: {rollback_error}; operator recovery required"));
            }
        }
    }
    // A failed final audit must not undo a successfully restored data plane.
    store.audit("restored", applied.state.revision, &applied.digest)
        .context("host restored but final audit failed; host state retained")?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DesiredPeer, DesiredState};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[derive(Default)]
    struct FakeBackend { live: bool, fail_activate: bool, rollback_count: usize, snapshot_count: usize }
    impl HostBackend for FakeBackend {
        type Snapshot = ();
        fn snapshot(&mut self) -> Result<()> {
            self.snapshot_count += 1;
            ensure!(!self.live, "existing tunnel");
            Ok(())
        }
        fn stage(&mut self, _: &DesiredState) -> Result<()> { Ok(()) }
        fn activate(&mut self) -> Result<()> {
            self.live = true;
            ensure!(!self.fail_activate, "injected activation failure");
            Ok(())
        }
        fn verify(&mut self, _: &DesiredState) -> Result<()> { ensure!(self.live, "missing tunnel"); Ok(()) }
        fn rollback(&mut self, _: ()) -> Result<()> {
            self.live = false;
            self.rollback_count += 1;
            Ok(())
        }
    }

    fn state() -> DesiredState {
        DesiredState { revision: 7, server_public_key: STANDARD.encode([1u8; 32]),
            peers: vec![DesiredPeer { public_key: STANDARD.encode([2u8; 32]),
                vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true,
                preshared_key: None, persistent_keepalive: 30 }] }
    }

    #[test]
    fn restores_without_advancing_revision_and_rolls_back_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        store.record_applied(state()).unwrap();
        let mut backend = FakeBackend::default();
        assert_eq!(restore_from_journal(&store, &mut backend).unwrap().state.revision, 7);
        assert!(backend.live);
        assert_eq!(store.load().unwrap().unwrap().state.revision, 7);

        let mut failed = FakeBackend { fail_activate: true, ..Default::default() };
        assert!(restore_from_journal(&store, &mut failed).is_err());
        assert!(!failed.live);
        assert_eq!(failed.rollback_count, 1);
        assert_eq!(store.load().unwrap().unwrap().state.revision, 7);
    }

    #[test]
    fn missing_journal_never_touches_host() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state"));
        let mut backend = FakeBackend::default();
        assert!(restore_from_journal(&store, &mut backend).is_err());
        assert_eq!(backend.snapshot_count, 0);
    }
}
