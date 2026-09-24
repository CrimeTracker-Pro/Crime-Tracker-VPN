//! First-cutover transaction boundary. No production HostOperations implementation exists.

use crate::apply::HostBackend;
use crate::firewall_executor::{FirewallCommands, FirewallRunner};
use crate::firewall_preflight::FirewallFacts;
use crate::policy::{BridgeRoute, HostForwardPlan, compile_host_forwarding};
use crate::state::DesiredState;
use crate::wireguard_config::{SensitiveWgConfig, render, verify_server_identity};
use anyhow::{Context, Result, ensure};
use zeroize::Zeroizing;

/// Implementations must never log or persist the secret-bearing configuration.
/// The first-cutover backend is deliberately unable to replace an existing wg0.
pub trait HostOperations: FirewallRunner {
    /// Re-observe iptables-save and the route, and compare the resulting plan.
    /// Also require the old UDP publish to be absent at the time of this call.
    fn require_current_network_plan(&mut self, expected: &HostForwardPlan) -> Result<()>;
    fn require_absent_wg0(&mut self) -> Result<()>;
    fn require_absent_owned_firewall(&mut self) -> Result<()>;
    fn activate_wg0(&mut self, config: &SensitiveWgConfig) -> Result<()>;
    fn verify_wg0(&mut self, state: &DesiredState) -> Result<()>;
    fn remove_wg0(&mut self) -> Result<()>;
}

pub struct HostCutoverBackend<O> {
    operations: O,
    network_plan: HostForwardPlan,
    firewall: FirewallCommands,
    server_private_key: Zeroizing<String>,
    server_public_key: String,
    staged: Option<SensitiveWgConfig>,
    firewall_installed: bool,
    wg_activation_attempted: bool,
}

impl<O: HostOperations> HostCutoverBackend<O> {
    /// The key match is verified before a backend can be constructed.
    pub async fn new(operations: O, facts: &FirewallFacts, route: &BridgeRoute,
        private_key: String, public_key: String) -> Result<Self> {
        let network_plan = compile_host_forwarding(facts, route)?;
        let private_key = Zeroizing::new(private_key);
        verify_server_identity(&private_key, &public_key).await?;
        let firewall = FirewallCommands::from_plan(&network_plan)?;
        Ok(Self { operations, network_plan, firewall, server_private_key: private_key,
            server_public_key: public_key, staged: None, firewall_installed: false,
            wg_activation_attempted: false })
    }

    pub fn operations(&self) -> &O { &self.operations }
}

impl<O: HostOperations> HostBackend for HostCutoverBackend<O> {
    type Snapshot = ();

    fn snapshot(&mut self) -> Result<Self::Snapshot> {
        ensure!(!self.firewall_installed && !self.wg_activation_attempted,
            "cutover backend is not reusable");
        self.operations.require_current_network_plan(&self.network_plan)?;
        self.operations.require_absent_wg0()?;
        self.operations.require_absent_owned_firewall()?;
        Ok(())
    }

    fn stage(&mut self, candidate: &DesiredState) -> Result<()> {
        ensure!(candidate.server_public_key == self.server_public_key,
            "server identity does not match verified key");
        self.staged = Some(render(candidate, &self.server_private_key)?);
        Ok(())
    }

    fn activate(&mut self) -> Result<()> {
        let config = self.staged.as_ref().context("candidate was not staged")?;
        self.operations.require_current_network_plan(&self.network_plan)?;
        self.operations.require_absent_wg0()?;
        self.operations.require_absent_owned_firewall()?;
        self.firewall.install(&mut self.operations)?;
        self.firewall_installed = true;
        self.wg_activation_attempted = true;
        self.operations.activate_wg0(config)?;
        Ok(())
    }

    fn verify(&mut self, candidate: &DesiredState) -> Result<()> {
        ensure!(self.firewall_installed && self.wg_activation_attempted,
            "cutover was not activated");
        self.operations.verify_wg0(candidate)
    }

    fn rollback(&mut self, _: Self::Snapshot) -> Result<()> {
        // If wg0 removal fails, keep the forwarding restrictions in place.
        if self.wg_activation_attempted {
            self.operations.remove_wg0().context("wg0 removal failed; firewall retained")?;
            self.wg_activation_attempted = false;
        }
        if self.firewall_installed {
            self.firewall.uninstall(&mut self.operations)?;
            self.firewall_installed = false;
        }
        self.staged = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firewall_executor::FirewallCommand;
    use crate::state::DesiredPeer;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[derive(Default)]
    struct FakeHost { events: Vec<&'static str>, fail_wg: bool, fail_remove: bool, wg_present: bool, network_changed: bool }
    impl FirewallRunner for FakeHost {
        fn run(&mut self, _: &FirewallCommand) -> Result<()> { self.events.push("firewall"); Ok(()) }
    }
    impl HostOperations for FakeHost {
        fn require_current_network_plan(&mut self, _: &HostForwardPlan) -> Result<()> {
            ensure!(!self.network_changed, "network plan changed");
            Ok(())
        }
        fn require_absent_wg0(&mut self) -> Result<()> { ensure!(!self.wg_present, "wg0 exists"); Ok(()) }
        fn require_absent_owned_firewall(&mut self) -> Result<()> { Ok(()) }
        fn activate_wg0(&mut self, _: &SensitiveWgConfig) -> Result<()> {
            self.events.push("activate_wg");
            self.wg_present = true;
            ensure!(!self.fail_wg, "injected wg failure");
            Ok(())
        }
        fn verify_wg0(&mut self, _: &DesiredState) -> Result<()> { self.events.push("verify_wg"); Ok(()) }
        fn remove_wg0(&mut self) -> Result<()> {
            self.events.push("remove_wg");
            ensure!(!self.fail_remove, "injected removal failure");
            self.wg_present = false;
            Ok(())
        }
    }

    fn backend(host: FakeHost) -> HostCutoverBackend<FakeHost> {
        let facts = crate::firewall_preflight::parse_iptables_save(
            "*filter\n:INPUT ACCEPT [0:0]\n:FORWARD DROP [0:0]\n-A FORWARD -j DOCKER-USER\nCOMMIT\n*nat\n-A DOCKER ! -i br-web -p tcp --dport 443 -j DNAT --to-destination 172.18.0.2:443\nCOMMIT\n").unwrap();
        let route = BridgeRoute { interface: "br-web".into(), source_ip: "172.18.0.1".parse().unwrap() };
        let plan = FirewallCommands::from_plan(&compile_host_forwarding(&facts, &route).unwrap()).unwrap();
        let network_plan = compile_host_forwarding(&facts, &route).unwrap();
        HostCutoverBackend { operations: host, network_plan, firewall: plan,
            server_private_key: Zeroizing::new(STANDARD.encode([1u8; 32])),
            server_public_key: STANDARD.encode([2u8; 32]), staged: None,
            firewall_installed: false, wg_activation_attempted: false }
    }

    fn state() -> DesiredState {
        DesiredState { revision: 1, server_public_key: STANDARD.encode([2u8; 32]),
            peers: vec![DesiredPeer { public_key: STANDARD.encode([3u8; 32]),
                vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true,
                preshared_key: None, persistent_keepalive: 30 }] }
    }

    #[test]
    fn activation_and_rollback_order() {
        let mut backend = backend(FakeHost { fail_wg: true, ..Default::default() });
        backend.snapshot().unwrap();
        backend.stage(&state()).unwrap();
        assert!(backend.activate().is_err());
        backend.rollback(()).unwrap();
        let events = &backend.operations().events;
        let wg_at = events.iter().position(|e| *e == "activate_wg").unwrap();
        let remove_at = events.iter().position(|e| *e == "remove_wg").unwrap();
        assert!(events[..wg_at].iter().all(|e| *e == "firewall"));
        assert_eq!(events[remove_at + 1], "firewall");
        assert!(!backend.operations().wg_present);
    }

    #[test]
    fn failed_wg_removal_keeps_firewall() {
        let mut backend = backend(FakeHost { fail_remove: true, ..Default::default() });
        backend.snapshot().unwrap();
        backend.stage(&state()).unwrap();
        backend.activate().unwrap();
        assert!(backend.rollback(()).unwrap_err().to_string().contains("firewall retained"));
        assert!(backend.firewall_installed);
        assert_eq!(backend.operations().events.last(), Some(&"remove_wg"));
    }

    #[test]
    fn existing_host_interface_blocks_snapshot() {
        let mut backend = backend(FakeHost { wg_present: true, ..Default::default() });
        assert!(backend.snapshot().is_err());
        assert!(backend.operations().events.is_empty());
    }

    #[test]
    fn changed_network_blocks_activation_before_firewall_mutation() {
        let mut backend = backend(FakeHost::default());
        backend.snapshot().unwrap();
        backend.stage(&state()).unwrap();
        backend.operations.network_changed = true;
        assert!(backend.activate().is_err());
        assert!(backend.operations().events.is_empty());
    }
}
