//! Read-only, repeatable host checks immediately before first cutover.

use crate::firewall_preflight::parse_iptables_save;
use crate::host_commands::{HostCommandRunner, SystemHostCommandRunner};
use crate::policy::{HostForwardPlan, compile_host_forwarding, parse_ipv4_route_get};
use anyhow::{Context, Result, ensure};
use std::net::Ipv4Addr;

pub trait HostProbe {
    fn iptables_save(&mut self) -> Result<String>;
    fn route_get(&mut self, target: Ipv4Addr) -> Result<String>;
    fn udp_listeners(&mut self) -> Result<String>;
    fn wg0_exists(&mut self) -> Result<bool>;
}

/// Executes discovery commands only; no shell or network mutation.
pub struct SystemHostProbe;

impl SystemHostProbe {
    fn output(program: &str, args: &[&str]) -> Result<String> {
        SystemHostCommandRunner::default().output(program, args)
            .with_context(|| format!("read-only {program} failed"))
    }
}

impl HostProbe for SystemHostProbe {
    fn iptables_save(&mut self) -> Result<String> { Self::output("iptables-save", &[]) }
    fn route_get(&mut self, target: Ipv4Addr) -> Result<String> {
        Self::output("ip", &["-4", "route", "get", &target.to_string()])
    }
    fn udp_listeners(&mut self) -> Result<String> { Self::output("ss", &["-H", "-lun"]) }
    fn wg0_exists(&mut self) -> Result<bool> {
        match std::fs::symlink_metadata("/sys/class/net/wg0") {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

pub fn require_first_cutover_ready(probe: &mut impl HostProbe, expected: &HostForwardPlan) -> Result<()> {
    ensure!(!probe.wg0_exists()?, "host wg0 already exists");
    let listeners = probe.udp_listeners()?;
    ensure!(!udp_port_listed(&listeners, 51820)?, "UDP 51820 already has a host listener");
    let snapshot = probe.iptables_save()?;
    ensure!(!snapshot.contains("ZEROVPN-HOST-FWD") && !snapshot.contains("ZEROVPN-HOST-NAT"),
        "VPN-owned firewall chains or jumps already exist");
    let facts = parse_iptables_save(&snapshot)?;
    let route_text = probe.route_get(expected.https_target)?;
    let route = parse_ipv4_route_get(&route_text, expected.https_target)?;
    let actual = compile_host_forwarding(&facts, &route)?;
    ensure!(&actual == expected, "Docker HTTPS route or firewall plan changed");
    Ok(())
}

fn udp_port_listed(output: &str, port: u16) -> Result<bool> {
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() >= 5 && fields[0] == "UNCONN", "unrecognized UDP listener output");
        let local_port = fields[3].rsplit(':').next().context("UDP local port missing")?;
        if local_port.parse::<u16>().context("invalid UDP local port")? == port {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::BridgeRoute;

    const SNAPSHOT: &str = "*filter\n:INPUT ACCEPT [0:0]\n:FORWARD DROP [0:0]\n-A FORWARD -j DOCKER-USER\nCOMMIT\n*nat\n-A DOCKER ! -i br-web -p tcp --dport 443 -j DNAT --to-destination 172.18.0.2:443\nCOMMIT\n";
    struct FakeProbe { firewall: String, route: String, listeners: String, wg0: bool }
    impl HostProbe for FakeProbe {
        fn iptables_save(&mut self) -> Result<String> { Ok(self.firewall.clone()) }
        fn route_get(&mut self, _: Ipv4Addr) -> Result<String> { Ok(self.route.clone()) }
        fn udp_listeners(&mut self) -> Result<String> { Ok(self.listeners.clone()) }
        fn wg0_exists(&mut self) -> Result<bool> { Ok(self.wg0) }
    }
    fn fixture() -> (FakeProbe, HostForwardPlan) {
        let facts = parse_iptables_save(SNAPSHOT).unwrap();
        let route = BridgeRoute { interface: "br-web".into(), source_ip: "172.18.0.1".parse().unwrap() };
        let plan = compile_host_forwarding(&facts, &route).unwrap();
        (FakeProbe { firewall: SNAPSHOT.into(),
            route: "172.18.0.2 dev br-web src 172.18.0.1\n".into(),
            listeners: "UNCONN 0 0 0.0.0.0:5353 0.0.0.0:*\n".into(), wg0: false }, plan)
    }

    #[test]
    fn clean_snapshot_passes() {
        let (mut probe, plan) = fixture();
        require_first_cutover_ready(&mut probe, &plan).unwrap();
    }

    #[test]
    fn occupied_port_existing_interface_or_owned_chain_blocks() {
        let (mut probe, plan) = fixture();
        probe.listeners = "UNCONN 0 0 [::]:51820 [::]:*\n".into();
        assert!(require_first_cutover_ready(&mut probe, &plan).is_err());
        probe.listeners.clear();
        probe.wg0 = true;
        assert!(require_first_cutover_ready(&mut probe, &plan).is_err());
        probe.wg0 = false;
        probe.firewall.push_str(":ZEROVPN-HOST-FWD - [0:0]\n");
        assert!(require_first_cutover_ready(&mut probe, &plan).is_err());
    }

    #[test]
    fn docker_publish_or_route_change_blocks() {
        let (mut probe, plan) = fixture();
        probe.firewall = probe.firewall.replacen(
            "*nat\n",
            "*nat\n-A DOCKER ! -i br-vpn -p udp --dport 51820 -j DNAT --to-destination 172.19.0.2:51820\n",
            1,
        );
        assert!(require_first_cutover_ready(&mut probe, &plan).is_err());
        let (mut probe, plan) = fixture();
        probe.route = "172.18.0.2 dev br-new src 172.18.0.1\n".into();
        assert!(require_first_cutover_ready(&mut probe, &plan).is_err());
    }

}
