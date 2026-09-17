use askama::Template;

/// Renders a peer-side `wg-quick` config.
#[derive(Template)]
#[template(
    source = r#"[Interface]
PrivateKey = {{ private_key }}
Address = {{ address }}
{% if mtu.is_some() %}
MTU = {{ mtu.unwrap() }}{% endif %}

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
