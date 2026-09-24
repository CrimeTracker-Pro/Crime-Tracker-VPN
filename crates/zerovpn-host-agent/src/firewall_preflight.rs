//! Read-only checks of `iptables-save` before taking ownership of host wg0.
//! Docker's iptables-nft tables are never modified by this module.

use anyhow::{Result, ensure};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FirewallFacts {
    pub input_accept: bool,
    pub forward_drop: bool,
    pub docker_user_hook: bool,
    pub old_container_udp_51820_publish: bool,
    pub https_443_publish: bool,
    /// Docker's published host:443 destination, discovered from NAT rules.
    pub https_target: Option<Ipv4Addr>,
    /// The bridge excluded by Docker's published-port DNAT rule.
    pub https_bridge: Option<String>,
}

/// Accepts `iptables-save` text only. This is intentionally strict: if Docker
/// changes the chain layout, activation must stop for operator review.
pub fn parse_iptables_save(snapshot: &str) -> Result<FirewallFacts> {
    let mut section = "";
    let mut facts = FirewallFacts {
        input_accept: false,
        forward_drop: false,
        docker_user_hook: false,
        old_container_udp_51820_publish: false,
        https_443_publish: false,
        https_target: None,
        https_bridge: None,
    };
    let mut saw_filter = false;
    let mut saw_nat = false;
    for line in snapshot.lines().map(str::trim) {
        match line {
            "*filter" => { section = "filter"; saw_filter = true; continue; }
            "*nat" => { section = "nat"; saw_nat = true; continue; }
            "COMMIT" => { section = ""; continue; }
            value if value.starts_with('*') => { section = ""; continue; }
            _ => {}
        }
        if section == "filter" {
            facts.input_accept |= line.starts_with(":INPUT ACCEPT ");
            facts.forward_drop |= line.starts_with(":FORWARD DROP ");
            facts.docker_user_hook |= line == "-A FORWARD -j DOCKER-USER";
        } else if section == "nat" && line.starts_with("-A DOCKER ") {
            let tokens: Vec<_> = line.split_whitespace().collect();
            let is_dnat = tokens.windows(2).any(|pair| pair == ["-j", "DNAT"]);
            let udp = tokens.windows(2).any(|pair| pair == ["-p", "udp"]);
            let tcp = tokens.windows(2).any(|pair| pair == ["-p", "tcp"]);
            let port_51820 = tokens.windows(2).any(|pair| pair == ["--dport", "51820"]);
            let port_443 = tokens.windows(2).any(|pair| pair == ["--dport", "443"]);
            facts.old_container_udp_51820_publish |= is_dnat && udp && port_51820;
            facts.https_443_publish |= is_dnat && tcp && port_443;
            if is_dnat && tcp && port_443 {
                let destination = tokens.windows(2)
                    .find(|pair| pair[0] == "--to-destination")
                    .and_then(|pair| pair[1].rsplit_once(':'));
                let bridge = tokens.windows(3)
                    .find(|triple| triple[0] == "!" && triple[1] == "-i")
                    .map(|triple| triple[2]);
                if let Some((ip, port)) = destination {
                    if port == "443" {
                        facts.https_target = ip.parse().ok();
                    }
                }
                facts.https_bridge = bridge.map(str::to_owned);
            }
        }
    }
    ensure!(saw_filter && saw_nat, "incomplete firewall snapshot");
    Ok(facts)
}

/// Gate for the future host activation path. It is expected to reject the
/// current production snapshot until the old API's UDP publish is removed.
pub fn require_host_activation_ready(facts: &FirewallFacts) -> Result<()> {
    ensure!(facts.input_accept, "host INPUT is not ACCEPT; local access needs a reviewed rule");
    ensure!(facts.forward_drop, "host FORWARD policy changed; review forwarding isolation");
    ensure!(facts.docker_user_hook, "Docker user forwarding hook missing");
    ensure!(!facts.old_container_udp_51820_publish, "old container still owns published UDP 51820");
    ensure!(facts.https_443_publish, "existing HTTPS publish missing; review Traefik path");
    ensure!(facts.https_target.is_some() && facts.https_bridge.is_some(), "HTTPS Docker target unresolved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = "*filter\n:INPUT ACCEPT [0:0]\n:FORWARD DROP [0:0]\n-A FORWARD -j DOCKER-USER\nCOMMIT\n*nat\n-A DOCKER ! -i br-vpn -p udp -m udp --dport 51820 -j DNAT --to-destination 172.19.0.2:51820\n-A DOCKER ! -i br-web -p tcp -m tcp --dport 443 -j DNAT --to-destination 172.18.0.2:443\nCOMMIT\n";

    #[test]
    fn live_container_publish_blocks_host_activation() {
        let facts = parse_iptables_save(SNAPSHOT).unwrap();
        assert!(facts.input_accept && facts.forward_drop && facts.docker_user_hook);
        assert!(facts.old_container_udp_51820_publish);
        assert!(facts.https_443_publish);
        assert_eq!(facts.https_target, Some("172.18.0.2".parse().unwrap()));
        assert_eq!(facts.https_bridge.as_deref(), Some("br-web"));
        assert!(require_host_activation_ready(&facts).is_err());
    }

    #[test]
    fn only_reviewed_post_cutover_snapshot_passes() {
        let without_old_publish = SNAPSHOT.lines()
            .filter(|line| !line.contains("--dport 51820"))
            .collect::<Vec<_>>().join("\n");
        let facts = parse_iptables_save(&without_old_publish).unwrap();
        assert!(require_host_activation_ready(&facts).is_ok());
        let missing_hook = without_old_publish.replace("-A FORWARD -j DOCKER-USER", "");
        assert!(require_host_activation_ready(&parse_iptables_save(&missing_hook).unwrap()).is_err());
    }

    #[test]
    fn missing_sections_fail_closed() {
        assert!(parse_iptables_save("*filter\n:INPUT ACCEPT [0:0]\nCOMMIT\n").is_err());
    }
}
