//! Scoped firewall commands for a future, guarded host cutover. Not connected to the API.

use crate::policy::HostForwardPlan;
use anyhow::{Context, Result, ensure};

const FILTER_CHAIN: &str = "ZEROVPN-HOST-FWD";
const NAT_CHAIN: &str = "ZEROVPN-HOST-NAT";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FirewallCommand {
    pub args: Vec<String>,
}

impl FirewallCommand {
    fn new(args: &[&str]) -> Self {
        Self { args: std::iter::once("-w".to_owned()).chain(args.iter().map(|s| (*s).to_owned())).collect() }
    }
}

pub trait FirewallRunner {
    fn run(&mut self, command: &FirewallCommand) -> Result<()>;
}

/// All commands target dedicated chains. Docker-owned chains are only given
/// narrow jumps; they are never flushed or replaced.
pub struct FirewallCommands {
    pub stage: Vec<FirewallCommand>,
    pub activate: Vec<FirewallCommand>,
    pub detach: Vec<FirewallCommand>,
    pub cleanup: Vec<FirewallCommand>,
}

impl FirewallCommands {
    pub fn from_plan(plan: &HostForwardPlan) -> Result<Self> {
        ensure!(plan.peer_to_peer_cidr == "10.0.0.0/22" && plan.drop_other_wg_forward,
            "unsupported forwarding policy");
        ensure!(!plan.https_bridge.is_empty() && plan.https_bridge.len() <= 15
            && plan.https_bridge.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
            "invalid bridge interface");
        let cidr = plan.peer_to_peer_cidr;
        let target = format!("{}/32", plan.https_target);
        let source = plan.https_snat_source.to_string();
        let bridge = plan.https_bridge.as_str();
        let nat_match = ["-s", cidr, "-d", &target,
            "-o", bridge, "-p", "tcp", "--dport", "443", "-j", NAT_CHAIN];
        let filter_jump = ["DOCKER-USER", "-i", "wg0", "-j", FILTER_CHAIN];
        let stage = vec![
            FirewallCommand::new(&["-N", FILTER_CHAIN]),
            FirewallCommand::new(&["-t", "nat", "-N", NAT_CHAIN]),
            FirewallCommand::new(&["-A", FILTER_CHAIN, "-i", "wg0", "-o", "wg0", "-s", cidr, "-d", cidr, "-j", "ACCEPT"]),
            FirewallCommand::new(&["-A", FILTER_CHAIN, "-i", "wg0", "-o", bridge, "-s", cidr, "-d", &target, "-p", "tcp", "--dport", "443", "-j", "ACCEPT"]),
            FirewallCommand::new(&["-A", FILTER_CHAIN, "-i", "wg0", "-j", "DROP"]),
            FirewallCommand::new(&["-t", "nat", "-A", NAT_CHAIN, "-s", cidr, "-d", &target, "-o", bridge, "-p", "tcp", "--dport", "443", "-j", "SNAT", "--to-source", &source]),
        ];
        // Build argv as separate tokens, never through a shell.
        let mut nat_insert = vec!["-t", "nat", "-I", "POSTROUTING", "1"];
        nat_insert.extend_from_slice(&nat_match);
        let mut nat_delete = vec!["-t", "nat", "-D", "POSTROUTING"];
        nat_delete.extend_from_slice(&nat_match);
        let mut filter_insert = vec!["-I", "DOCKER-USER", "1"];
        filter_insert.extend_from_slice(&filter_jump[1..]);
        let mut filter_delete = vec!["-D", "DOCKER-USER"];
        filter_delete.extend_from_slice(&filter_jump[1..]);
        Ok(Self {
            stage,
            activate: vec![FirewallCommand::new(&nat_insert), FirewallCommand::new(&filter_insert)],
            detach: vec![FirewallCommand::new(&filter_delete), FirewallCommand::new(&nat_delete)],
            cleanup: vec![
                FirewallCommand::new(&["-F", FILTER_CHAIN]),
                FirewallCommand::new(&["-X", FILTER_CHAIN]),
                FirewallCommand::new(&["-t", "nat", "-F", NAT_CHAIN]),
                FirewallCommand::new(&["-t", "nat", "-X", NAT_CHAIN]),
            ],
        })
    }

    /// Cleanup errors are surfaced alongside the primary failure; never hidden.
    pub fn install(&self, runner: &mut impl FirewallRunner) -> Result<()> {
        let mut staged = 0;
        for command in &self.stage {
            if let Err(error) = runner.run(command) {
                let cleanup = self.cleanup_stage(runner, staged);
                return Err(error.context(format!("firewall staging failed; cleanup: {cleanup:?}")));
            }
            staged += 1;
        }
        let mut attached = 0;
        for command in &self.activate {
            if let Err(error) = runner.run(command) {
                let detach = self.detach_attached(runner, attached);
                if !detach.is_empty() {
                    return Err(error.context(format!("firewall activation failed; detach failed: {detach:?}; chains retained for operator recovery")));
                }
                let cleanup = self.cleanup_stage(runner, staged);
                return Err(error.context(format!("firewall activation failed; cleanup: {cleanup:?}")));
            }
            attached += 1;
        }
        Ok(())
    }

    /// Remove only this generation's exact jumps, then its private chains.
    /// A failed detach halts cleanup so referenced rules remain inspectable.
    pub fn uninstall(&self, runner: &mut impl FirewallRunner) -> Result<()> {
        for command in &self.detach {
            runner.run(command).context("firewall detach failed; chains retained for operator recovery")?;
        }
        for command in &self.cleanup {
            runner.run(command).context("firewall chain cleanup failed; operator recovery required")?;
        }
        Ok(())
    }

    /// Read-only reconciliation for a restarted agent. Exact rules are
    /// checked with iptables `-C`; counts prevent accepting extra VPN-owned
    /// rules or duplicate jumps. Unrelated Docker/user rules are untouched.
    pub fn verify_installed(&self, runner: &mut impl FirewallRunner, snapshot: &str) -> Result<()> {
        let count = |needle: &str| snapshot.lines().filter(|line| line.contains(needle)).count();
        ensure!(count(":ZEROVPN-HOST-FWD ") == 1 && count(":ZEROVPN-HOST-NAT ") == 1,
            "VPN-owned chains missing or duplicated");
        ensure!(snapshot.lines().filter(|line| line.starts_with("-A ZEROVPN-HOST-FWD ")).count() == 3,
            "VPN forwarding chain has unexpected rules");
        ensure!(snapshot.lines().filter(|line| line.starts_with("-A ZEROVPN-HOST-NAT ")).count() == 1,
            "VPN NAT chain has unexpected rules");
        ensure!(snapshot.lines().filter(|line| line.starts_with("-A DOCKER-USER ") && line.contains("-j ZEROVPN-HOST-FWD")).count() == 1,
            "VPN forwarding jump missing or duplicated");
        ensure!(snapshot.lines().filter(|line| line.starts_with("-A POSTROUTING ") && line.contains("-j ZEROVPN-HOST-NAT")).count() == 1,
            "VPN NAT jump missing or duplicated");
        for command in self.stage.iter().skip(2).chain(self.activate.iter()) {
            let mut args = command.args.clone();
            let position = args.iter().position(|arg| arg == "-A" || arg == "-I")
                .expect("only append or insert rules after chain creation");
            let insert = args[position] == "-I";
            args[position] = "-C".into();
            if insert { args.remove(position + 2); } // Remove insertion index 1.
            runner.run(&FirewallCommand { args })?;
        }
        Ok(())
    }

    fn detach_attached(&self, runner: &mut impl FirewallRunner, count: usize) -> Vec<String> {
        self.detach.iter().skip(self.detach.len() - count).filter_map(|command| runner.run(command).err().map(|e| e.to_string())).collect()
    }

    fn cleanup_stage(&self, runner: &mut impl FirewallRunner, count: usize) -> Vec<String> {
        // A failed append may have modified its own chain, so flush any chain
        // that was successfully created. Never touch Docker-owned chains.
        let filter_exists = count > 0;
        let nat_exists = count > 1;
        self.cleanup.iter().enumerate().filter(|(i, _)| if *i < 2 { filter_exists } else { nat_exists })
            .filter_map(|(_, command)| runner.run(command).err().map(|e| e.to_string())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[derive(Default)]
    struct RecordingRunner { calls: Vec<FirewallCommand>, fail_at: Option<usize> }
    impl FirewallRunner for RecordingRunner {
        fn run(&mut self, command: &FirewallCommand) -> Result<()> {
            self.calls.push(command.clone());
            ensure!(self.fail_at != Some(self.calls.len()), "injected failure");
            Ok(())
        }
    }

    fn commands() -> FirewallCommands {
        FirewallCommands::from_plan(&HostForwardPlan {
            peer_to_peer_cidr: "10.0.0.0/22",
            https_target: Ipv4Addr::new(172, 18, 0, 2),
            https_bridge: "br-web".into(),
            https_snat_source: Ipv4Addr::new(172, 18, 0, 1),
            drop_other_wg_forward: true,
        }).unwrap()
    }

    #[test]
    fn stages_before_attaching_and_never_flushes_docker_chains() {
        let commands = commands();
        let mut runner = RecordingRunner::default();
        commands.install(&mut runner).unwrap();
        assert_eq!(runner.calls, [commands.stage, commands.activate].concat());
        assert!(runner.calls.iter().all(|c| !c.args.windows(2).any(|a| a == ["-F", "DOCKER-USER"] || a == ["-F", "POSTROUTING"])));
    }

    #[test]
    fn failed_filter_attach_detaches_nat_then_cleans_own_chains() {
        let commands = commands();
        let mut runner = RecordingRunner { fail_at: Some(commands.stage.len() + 2), ..Default::default() };
        assert!(commands.install(&mut runner).is_err());
        assert_eq!(runner.calls[commands.stage.len() + 2], commands.detach[1]);
        assert_eq!(&runner.calls[commands.stage.len() + 3..], &commands.cleanup);
    }

    #[test]
    fn failed_chain_creation_does_not_delete_preexisting_chains() {
        let commands = commands();
        let mut runner = RecordingRunner { fail_at: Some(1), ..Default::default() };
        assert!(commands.install(&mut runner).is_err());
        assert_eq!(runner.calls.len(), 1);
    }

    #[test]
    fn failed_detach_retains_chains_for_recovery() {
        let commands = commands();
        // Filter attachment fails, then NAT detach fails.
        struct MultiFail { calls: Vec<FirewallCommand>, fail: Vec<usize> }
        impl FirewallRunner for MultiFail {
            fn run(&mut self, command: &FirewallCommand) -> Result<()> {
                self.calls.push(command.clone());
                ensure!(!self.fail.contains(&self.calls.len()), "injected failure");
                Ok(())
            }
        }
        let mut runner = MultiFail { calls: vec![], fail: vec![commands.stage.len() + 2, commands.stage.len() + 3] };
        let error = commands.install(&mut runner).unwrap_err().to_string();
        assert!(error.contains("chains retained"));
        assert_eq!(runner.calls.len(), commands.stage.len() + 3);
    }

    #[test]
    fn uninstall_stops_if_jump_detach_fails() {
        let commands = commands();
        let mut runner = RecordingRunner { fail_at: Some(1), ..Default::default() };
        assert!(commands.uninstall(&mut runner).is_err());
        assert_eq!(runner.calls, vec![commands.detach[0].clone()]);
    }

    #[test]
    fn reconciliation_rejects_extra_owned_rules() {
        let commands = commands();
        let snapshot = "*filter\n:ZEROVPN-HOST-FWD - [0:0]\n-A DOCKER-USER -i wg0 -j ZEROVPN-HOST-FWD\n-A ZEROVPN-HOST-FWD -j ACCEPT\n-A ZEROVPN-HOST-FWD -j ACCEPT\n-A ZEROVPN-HOST-FWD -j DROP\nCOMMIT\n*nat\n:ZEROVPN-HOST-NAT - [0:0]\n-A POSTROUTING -j ZEROVPN-HOST-NAT\n-A ZEROVPN-HOST-NAT -j SNAT\nCOMMIT\n";
        let mut runner = RecordingRunner::default();
        // The fake runner accepts all checks; malformed extra rules must
        // still be rejected by the structural count gate.
        assert!(commands.verify_installed(&mut runner, &snapshot.replace(
            "-A ZEROVPN-HOST-FWD -j DROP", "-A ZEROVPN-HOST-FWD -j DROP\n-A ZEROVPN-HOST-FWD -j ACCEPT"
        )).is_err());
        assert!(runner.calls.is_empty());
    }
}
