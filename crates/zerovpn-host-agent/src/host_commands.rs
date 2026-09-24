//! First-cutover host commands. Deliberately not constructed by the socket service.

use crate::firewall_executor::{FirewallCommand, FirewallRunner};
use crate::host_cutover::HostOperations;
use crate::host_preflight::{HostProbe, require_first_cutover_ready};
use crate::policy::HostForwardPlan;
use crate::state::DesiredState;
use crate::wireguard_config::SensitiveWgConfig;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub trait HostCommandRunner {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<()>;
    fn output(&mut self, program: &str, args: &[&str]) -> Result<String>;
}

/// No shell is used. The command's stderr is discarded because `wg` may
/// include sensitive configuration in diagnostics. Callers get only a generic
/// command failure; the secret is never put in argv or an environment variable.
pub struct SystemHostCommandRunner { timeout: Duration }

impl Default for SystemHostCommandRunner {
    fn default() -> Self { Self { timeout: Duration::from_secs(10) } }
}

impl SystemHostCommandRunner {
    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        ensure!(!timeout.is_zero(), "command timeout must be positive");
        Ok(Self { timeout })
    }

    fn wait_bounded(&self, child: &mut Child) -> Result<ExitStatus> {
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("host command timed out and was terminated");
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        }
    }
}

impl HostCommandRunner for SystemHostCommandRunner {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<()> {
        let mut child = Command::new(program).args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn()
            .with_context(|| format!("could not start {program}"))?;
        let input = child.stdin.take();
        std::thread::scope(|scope| -> Result<()> {
            let writer = match (input, stdin) {
                (Some(mut pipe), Some(bytes)) => Some(scope.spawn(move || pipe.write_all(bytes))),
                (None, None) => None,
                _ => anyhow::bail!("command stdin mismatch"),
            };
            let status = self.wait_bounded(&mut child);
            if let Some(writer) = writer {
                let write_result = writer.join().map_err(|_| anyhow::anyhow!("stdin writer panicked"))?;
                write_result.context("could not send protected configuration")?;
            }
            ensure!(status?.success(), "{program} command failed");
            Ok(())
        })?;
        Ok(())
    }

    fn output(&mut self, program: &str, args: &[&str]) -> Result<String> {
        let mut child = Command::new(program).args(args).stdin(Stdio::null())
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let stdout = child.stdout.take().context("command stdout missing")?;
        std::thread::scope(|scope| -> Result<String> {
            let reader = scope.spawn(move || {
                let mut bytes = Vec::new();
                stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                Ok::<_, std::io::Error>(bytes)
            });
            let status = self.wait_bounded(&mut child);
            let bytes = reader.join().map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
            ensure!(bytes.len() <= 1024 * 1024, "host observation output too large");
            ensure!(status?.success(), "{program} observation failed");
            Ok(String::from_utf8(bytes)?)
        })
    }
}

/// The ownership bit becomes true only after `ip link add` succeeds, so a
/// failed add cannot cause rollback to delete somebody else's interface.
pub struct CommandHostOperations<R, P> {
    runner: R,
    probe: P,
    wg0_created: bool,
}

impl<R, P> CommandHostOperations<R, P> {
    pub fn new(runner: R, probe: P) -> Self { Self { runner, probe, wg0_created: false } }
    pub fn runner(&self) -> &R { &self.runner }
}

impl<R: HostCommandRunner, P> FirewallRunner for CommandHostOperations<R, P> {
    fn run(&mut self, command: &FirewallCommand) -> Result<()> {
        let args: Vec<_> = command.args.iter().map(String::as_str).collect();
        self.runner.run("iptables", &args, None)
    }
}

impl<R: HostCommandRunner, P: HostProbe> HostOperations for CommandHostOperations<R, P> {
    fn require_current_network_plan(&mut self, expected: &HostForwardPlan) -> Result<()> {
        require_first_cutover_ready(&mut self.probe, expected)
    }

    fn require_absent_wg0(&mut self) -> Result<()> {
        ensure!(!self.probe.wg0_exists()?, "host wg0 already exists");
        Ok(())
    }

    fn require_absent_owned_firewall(&mut self) -> Result<()> {
        let snapshot = self.probe.iptables_save()?;
        ensure!(!snapshot.contains("ZEROVPN-HOST-FWD") && !snapshot.contains("ZEROVPN-HOST-NAT"),
            "VPN-owned firewall rules already exist");
        Ok(())
    }

    fn activate_wg0(&mut self, config: &SensitiveWgConfig) -> Result<()> {
        self.runner.run("ip", &["link", "add", "dev", "wg0", "type", "wireguard"], None)?;
        self.wg0_created = true;
        self.runner.run("ip", &["address", "add", "10.0.0.1/22", "dev", "wg0"], None)?;
        self.runner.run("wg", &["setconf", "wg0", "/dev/stdin"], Some(config.as_bytes()))?;
        self.runner.run("ip", &["link", "set", "dev", "wg0", "up"], None)?;
        Ok(())
    }

    fn verify_wg0(&mut self, state: &DesiredState) -> Result<()> {
        ensure!(self.wg0_created, "agent has not created wg0");
        verify_wireguard_state(&mut self.runner, state)
    }

    fn remove_wg0(&mut self) -> Result<()> {
        if self.wg0_created {
            self.runner.run("ip", &["link", "delete", "dev", "wg0"], None)?;
            self.wg0_created = false;
        }
        Ok(())
    }
}

/// Read-only verification also usable after process restart; it does not
/// assume that this process created the interface.
pub fn verify_wireguard_state(runner: &mut impl HostCommandRunner, state: &DesiredState) -> Result<()> {
        let public_key = runner.output("wg", &["show", "wg0", "public-key"])?;
        ensure!(public_key.trim() == state.server_public_key, "server public key mismatch");
        let port = runner.output("wg", &["show", "wg0", "listen-port"])?;
        ensure!(port.trim() == "51820", "WireGuard listen port mismatch");
        let addresses = runner.output("ip", &["-4", "-o", "address", "show", "dev", "wg0"])?;
        ensure!(addresses.split_whitespace().any(|field| field == "10.0.0.1/22"),
            "host wg0 address mismatch");
        let allowed = runner.output("wg", &["show", "wg0", "allowed-ips"])?;
        let mut observed = BTreeMap::new();
        for line in allowed.lines() {
            let mut fields = line.split_whitespace();
            let key = fields.next().context("peer public key missing")?;
            let ip = fields.next().context("peer allowed IP missing")?;
            ensure!(fields.next().is_none(), "unexpected peer allowed IPs");
            ensure!(observed.insert(key.to_owned(), ip.to_owned()).is_none(), "duplicate observed peer");
        }
        let expected: BTreeMap<_, _> = state.peers.iter().filter(|peer| peer.enabled)
            .map(|peer| (peer.public_key.clone(), format!("{}/32", peer.vpn_ip))).collect();
        ensure!(observed == expected, "active WireGuard peers differ from desired state");
        let keepalive = runner.output("wg", &["show", "wg0", "persistent-keepalive"])?;
        let observed_keepalive = parse_peer_values(&keepalive)?;
        let preshared = Zeroizing::new(runner.output("wg", &["show", "wg0", "preshared-keys"])?);
        let observed_preshared = parse_peer_values(&preshared)?;
        ensure!(observed_keepalive.len() == expected.len() && observed_preshared.len() == expected.len(),
            "active WireGuard peer attributes differ from desired state");
        for peer in state.peers.iter().filter(|peer| peer.enabled) {
            let interval = observed_keepalive.get(peer.public_key.as_str())
                .context("peer keepalive missing")?;
            let parsed_interval = if *interval == "off" { 0 } else { interval.parse::<u16>()? };
            ensure!(parsed_interval == peer.persistent_keepalive, "peer keepalive mismatch");
            let actual_psk = observed_preshared.get(peer.public_key.as_str())
                .context("peer preshared key missing")?;
            let matches = match &peer.preshared_key {
                Some(expected_psk) => *actual_psk == expected_psk,
                None => *actual_psk == "(none)",
            };
            ensure!(matches, "peer preshared key mismatch");
        }
        Ok(())
}

fn parse_peer_values(output: &str) -> Result<BTreeMap<&str, &str>> {
    let mut values = BTreeMap::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let key = fields.next().context("peer key missing")?;
        let value = fields.next().context("peer value missing")?;
        ensure!(fields.next().is_none() && values.insert(key, value).is_none(),
            "invalid peer attribute output");
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_preflight::HostProbe;
    use crate::state::DesiredPeer;
    use crate::wireguard_config::render;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::net::Ipv4Addr;

    #[derive(Default)]
    struct RecordingRunner { calls: Vec<(String, Vec<String>, bool)>, fail_at: Option<usize>, key: String, peer: String }
    impl HostCommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<()> {
            self.calls.push((program.into(), args.iter().map(|s| (*s).into()).collect(), stdin.is_some()));
            ensure!(self.fail_at != Some(self.calls.len()), "injected failure");
            Ok(())
        }
        fn output(&mut self, program: &str, args: &[&str]) -> Result<String> {
            Ok(match (program, args) {
                ("wg", ["show", "wg0", "public-key"]) => self.key.clone(),
                ("wg", ["show", "wg0", "listen-port"]) => "51820".into(),
                ("wg", ["show", "wg0", "allowed-ips"]) => format!("{} 10.0.0.2/32\n", self.peer),
                ("wg", ["show", "wg0", "persistent-keepalive"]) => format!("{} 30\n", self.peer),
                ("wg", ["show", "wg0", "preshared-keys"]) => format!("{} (none)\n", self.peer),
                ("ip", ["-4", "-o", "address", "show", "dev", "wg0"]) => "9: wg0 inet 10.0.0.1/22 scope global wg0\n".into(),
                _ => anyhow::bail!("unexpected observation"),
            })
        }
    }
    struct NoopProbe;
    impl HostProbe for NoopProbe {
        fn iptables_save(&mut self) -> Result<String> { Ok(String::new()) }
        fn route_get(&mut self, _: Ipv4Addr) -> Result<String> { Ok(String::new()) }
        fn udp_listeners(&mut self) -> Result<String> { Ok(String::new()) }
        fn wg0_exists(&mut self) -> Result<bool> { Ok(false) }
    }
    fn state() -> DesiredState {
        DesiredState { revision: 1, server_public_key: STANDARD.encode([1u8; 32]),
            peers: vec![DesiredPeer { public_key: STANDARD.encode([2u8; 32]),
                vpn_ip: "10.0.0.2".parse().unwrap(), enabled: true,
                preshared_key: None, persistent_keepalive: 30 }] }
    }

    #[test]
    fn creates_configures_verifies_and_removes_only_owned_interface() {
        let state = state();
        let runner = RecordingRunner { key: state.server_public_key.clone(), peer: state.peers[0].public_key.clone(), ..Default::default() };
        let mut host = CommandHostOperations::new(runner, NoopProbe);
        let config = render(&state, &STANDARD.encode([3u8; 32])).unwrap();
        host.activate_wg0(&config).unwrap();
        host.verify_wg0(&state).unwrap();
        host.remove_wg0().unwrap();
        let calls = &host.runner().calls;
        assert_eq!(calls.iter().map(|(p, _, _)| p.as_str()).collect::<Vec<_>>(), vec!["ip", "ip", "wg", "ip", "ip"]);
        assert_eq!(calls[2].1, ["setconf", "wg0", "/dev/stdin"]);
        assert!(calls[2].2);
        assert!(calls.iter().all(|(_, args, _)| !args.iter().any(|a| a == &STANDARD.encode([3u8; 32]))));
    }

    #[test]
    fn failed_add_never_deletes_an_unowned_interface() {
        let mut host = CommandHostOperations::new(RecordingRunner { fail_at: Some(1), ..Default::default() }, NoopProbe);
        let config = render(&state(), &STANDARD.encode([3u8; 32])).unwrap();
        assert!(host.activate_wg0(&config).is_err());
        host.remove_wg0().unwrap();
        assert_eq!(host.runner().calls.len(), 1);
    }

    #[test]
    fn failed_configuration_can_remove_created_interface() {
        let mut host = CommandHostOperations::new(RecordingRunner { fail_at: Some(3), ..Default::default() }, NoopProbe);
        let config = render(&state(), &STANDARD.encode([3u8; 32])).unwrap();
        assert!(host.activate_wg0(&config).is_err());
        host.remove_wg0().unwrap();
        assert_eq!(host.runner().calls.last().unwrap().1, ["link", "delete", "dev", "wg0"]);
    }

    #[test]
    fn system_runner_bounds_command_and_observation_time() {
        let mut runner = SystemHostCommandRunner::with_timeout(Duration::from_millis(50)).unwrap();
        let start = Instant::now();
        assert!(runner.run("sleep", &["2"], None).is_err());
        assert!(runner.output("sleep", &["2"]).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
