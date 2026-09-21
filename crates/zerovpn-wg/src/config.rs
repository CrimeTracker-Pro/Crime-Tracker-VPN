use askama::Template;

/// Renders a peer-side `wg-quick` config.
#[derive(Template)]
#[template(
    source = r#"[Interface]
PrivateKey = {{ private_key }}
Address = {{ address }}
{% if mtu.is_some() %}MTU = {{ mtu.unwrap() }}{% endif %}

[Peer]
PublicKey = {{ server_public_key }}{% if let Some(psk) = preshared_key %}
PresharedKey = {{ psk }}{% endif %}
AllowedIPs = {{ allowed_ips }}
Endpoint = {{ endpoint }}
PersistentKeepalive = {{ keepalive }}
"#,
    ext = "txt",
    escape = "none"
)]
pub struct PeerConfig<'a> {
    pub private_key: &'a str,
    pub address: &'a str,
    pub mtu: Option<u16>,
    pub server_public_key: &'a str,
    pub preshared_key: Option<&'a str>,
    pub allowed_ips: &'a str,
    pub endpoint: &'a str,
    pub keepalive: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtu_follows_address_without_a_dns_placeholder_line() {
        let rendered = PeerConfig {
            private_key: "client-private",
            address: "10.0.0.2/32",
            mtu: Some(1420),
            server_public_key: "server-public",
            preshared_key: None,
            allowed_ips: "10.0.0.0/22",
            endpoint: "vpn.example.test:51820",
            keepalive: 30,
        }.render().expect("config renders");

        assert!(rendered.contains("Address = 10.0.0.2/32\nMTU = 1420\n\n[Peer]"));
        assert!(!rendered.contains("Address = 10.0.0.2/32\n\nMTU"));
    }
}
