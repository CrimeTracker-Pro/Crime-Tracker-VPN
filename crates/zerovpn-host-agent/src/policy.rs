//! Host INPUT intent only. This does not alter forwarding, NAT, or the live firewall.

use crate::state::DesiredState;
use crate::firewall_preflight::{FirewallFacts, require_host_activation_ready};
use anyhow::{Result, ensure};
use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use tokio::process::Command;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HostInputPolicy {
    /// Only authenticated, enabled WireGuard peer addresses are listed.
    pub source_ips: Vec<Ipv4Addr>,
    /// No per-service or per-port restriction for traffic entering the host
    /// from wg0. Other interfaces and FORWARD traffic are unaffected.
    pub allow_all_local_ports: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BridgeRoute {
    /// Interface returned by the host route to Docker's published HTTPS target.
    pub interface: String,
    pub source_ip: Ipv4Addr,
}

pub fn parse_ipv4_route_get(output: &str, target: Ipv4Addr) -> Result<BridgeRoute> {
    let first = output.lines().next().ok_or_else(|| anyhow::anyhow!("empty route output"))?;
    let tokens: Vec<_> = first.split_whitespace().collect();
    let reported_target: Ipv4Addr = tokens.first()
        .ok_or_else(|| anyhow::anyhow!("route target missing"))?.parse()?;
    ensure!(reported_target == target, "route target mismatch");
    ensure!(!tokens.contains(&"via"), "HTTPS target is not directly connected");
    let interface = tokens.windows(2).find(|pair| pair[0] == "dev")
        .map(|pair| pair[1]).ok_or_else(|| anyhow::anyhow!("route interface missing"))?;
    let source_ip: Ipv4Addr = tokens.windows(2).find(|pair| pair[0] == "src")
        .map(|pair| pair[1]).ok_or_else(|| anyhow::anyhow!("route source missing"))?.parse()?;
    Ok(BridgeRoute { interface: interface.to_owned(), source_ip })
}

/// Read-only discovery for the future activation preflight.
pub async fn observe_host_route(target: Ipv4Addr) -> Result<BridgeRoute> {
    let target_text = target.to_string();
    let output = Command::new("ip").args(["-4", "route", "get", &target_text])
        .kill_on_drop(true).output().await?;
    ensure!(output.status.success(), "could not inspect HTTPS route");
    parse_ipv4_route_get(std::str::from_utf8(&output.stdout)?, target)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HostForwardPlan {
    pub peer_to_peer_cidr: &'static str,
    pub https_target: Ipv4Addr,
    pub https_bridge: String,
    pub https_snat_source: Ipv4Addr,
    /// Every other packet entering FORWARD from wg0 remains denied.
    pub drop_other_wg_forward: bool,
}

pub fn compile_host_input(state: &DesiredState) -> Result<HostInputPolicy> {
    state.validate(state.revision.saturating_sub(1))?;
    let source_ips: BTreeSet<Ipv4Addr> = state.peers.iter()
        .filter(|peer| peer.enabled)
        .map(|peer| peer.vpn_ip)
        .collect();
    Ok(HostInputPolicy {
        source_ips: source_ips.into_iter().collect(),
        allow_all_local_ports: true,
    })
}

/// Resolve Docker's dynamic HTTPS target against the host route before
/// constructing any forwarding/NAT operations. This is intent, not execution.
pub fn compile_host_forwarding(facts: &FirewallFacts, route: &BridgeRoute) -> Result<HostForwardPlan> {
    require_host_activation_ready(facts)?;
    let bridge = facts.https_bridge.as_deref().expect("checked by preflight");
    let target = facts.https_target.expect("checked by preflight");
    ensure!(route.interface == bridge, "HTTPS route does not use Docker's published bridge");
    ensure!(route.interface.len() <= 15
        && !route.interface.is_empty()
        && route.interface.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "invalid Docker bridge interface name");
    ensure!(route.source_ip != target && !route.source_ip.is_unspecified()
        && !route.source_ip.is_loopback() && !route.source_ip.is_multicast(),
        "invalid HTTPS SNAT source");
    Ok(HostForwardPlan {
        peer_to_peer_cidr: "10.0.0.0/22",
        https_target: target,
        https_bridge: bridge.to_owned(),
        https_snat_source: route.source_ip,
        drop_other_wg_forward: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DesiredPeer;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    fn key(byte: u8) -> String { STANDARD.encode([byte; 32]) }

    #[test]
    fn every_enabled_peer_gets_unrestricted_local_host_access() {
        let state = DesiredState {
            revision: 1,
            server_public_key: key(1),
            peers: vec![
                DesiredPeer { public_key: key(2), vpn_ip: Ipv4Addr::new(10, 0, 3, 254), enabled: true, preshared_key: None, persistent_keepalive: 30 },
                DesiredPeer { public_key: key(3), vpn_ip: Ipv4Addr::new(10, 0, 0, 2), enabled: true, preshared_key: None, persistent_keepalive: 30 },
                DesiredPeer { public_key: key(4), vpn_ip: Ipv4Addr::new(10, 0, 0, 4), enabled: false, preshared_key: None, persistent_keepalive: 30 },
            ],
        };
        let policy = compile_host_input(&state).unwrap();
        assert!(policy.allow_all_local_ports);
        assert_eq!(policy.source_ips, vec![Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(10, 0, 3, 254)]);
        assert!(!policy.source_ips.contains(&Ipv4Addr::new(10, 0, 0, 4)));
    }

    #[test]
    fn forwarding_plan_preserves_peer_and_https_paths_only() {
        let snapshot = "*filter\n:INPUT ACCEPT [0:0]\n:FORWARD DROP [0:0]\n-A FORWARD -j DOCKER-USER\nCOMMIT\n*nat\n-A DOCKER ! -i br-web -p tcp --dport 443 -j DNAT --to-destination 172.18.0.2:443\nCOMMIT\n";
        let facts = crate::firewall_preflight::parse_iptables_save(snapshot).unwrap();
        let route = BridgeRoute { interface: "br-web".into(), source_ip: Ipv4Addr::new(172, 18, 0, 1) };
        let plan = compile_host_forwarding(&facts, &route).unwrap();
        assert_eq!(plan.peer_to_peer_cidr, "10.0.0.0/22");
        assert_eq!(plan.https_target, Ipv4Addr::new(172, 18, 0, 2));
        assert_eq!(plan.https_snat_source, Ipv4Addr::new(172, 18, 0, 1));
        assert!(plan.drop_other_wg_forward);
        let wrong_route = BridgeRoute { interface: "enp1s0".into(), source_ip: Ipv4Addr::new(192, 168, 1, 20) };
        assert!(compile_host_forwarding(&facts, &wrong_route).is_err());
        let observed = parse_ipv4_route_get(
            "172.18.0.2 dev br-web src 172.18.0.1 uid 1000\n    cache\n",
            Ipv4Addr::new(172, 18, 0, 2),
        ).unwrap();
        assert_eq!(observed, route);
        assert!(parse_ipv4_route_get(
            "172.18.0.2 via 192.168.1.1 dev enp1s0 src 192.168.1.20\n",
            Ipv4Addr::new(172, 18, 0, 2),
        ).is_err());
    }
}
