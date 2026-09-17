use ipnetwork::IpNetwork;
use uuid::Uuid;
use zerovpn_core::models::Server;

use crate::PgPool;

pub async fn list_active(pool: &PgPool) -> sqlx::Result<Vec<Server>> {
    sqlx::query_as::<_, Server>(
        r#"SELECT id, name, region, endpoint_host, endpoint_port, public_key,
                  cidr, dns_servers, mtu, is_active, persistent_keepalive
           FROM servers
           WHERE is_active = TRUE
           ORDER BY name"#,
    )
    .fetch_all(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Server>> {
    sqlx::query_as::<_, Server>(
        r#"SELECT id, name, region, endpoint_host, endpoint_port, public_key,
                  cidr, dns_servers, mtu, is_active, persistent_keepalive
           FROM servers
           WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub struct NewServer<'a> {
    pub name: &'a str,
    pub region: &'a str,
    pub endpoint_host: &'a str,
    pub endpoint_port: i32,
    pub public_key: &'a str,
    /// KEK-encrypted WG server private key (nonce-prefixed AES-GCM). Stored so
    /// the api can restore `wg0.conf` from the DB after a `wg_config` volume loss.
    pub private_key_encrypted: &'a [u8],
    pub cidr: IpNetwork,
    pub dns_servers: Vec<IpNetwork>,
    pub mtu: i32,
}

pub async fn create(pool: &PgPool, new: NewServer<'_>) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO servers (id, name, region, endpoint_host, endpoint_port,
                                public_key, private_key_encrypted, cidr, dns_servers, mtu, is_active)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, TRUE)"#,
    )
    .bind(id)
    .bind(new.name)
    .bind(new.region)
    .bind(new.endpoint_host)
    .bind(new.endpoint_port)
    .bind(new.public_key)
    .bind(new.private_key_encrypted)
    .bind(new.cidr)
    .bind(&new.dns_servers)
    .bind(new.mtu)
    .execute(pool)
    .await?;
    Ok(id)
}

/// `(id, private_key_encrypted, cidr)` for the `default` server, or `None` if it does
/// not exist yet. `private_key_encrypted` is `None` for rows created before the
/// key was stored in the DB (backfilled on the next boot).
pub async fn default_key_state(
    pool: &PgPool,
) -> sqlx::Result<Option<(Uuid, Option<Vec<u8>>, IpNetwork)>> {
    sqlx::query_as::<_, (Uuid, Option<Vec<u8>>, IpNetwork)>(
        "SELECT id, private_key_encrypted, cidr FROM servers WHERE name = 'default'",
    )
    .fetch_optional(pool)
    .await
}

/// Backfill the KEK-encrypted server private key onto an existing row.
pub async fn set_private_key(pool: &PgPool, id: Uuid, encrypted: &[u8]) -> sqlx::Result<()> {
    sqlx::query("UPDATE servers SET private_key_encrypted = $1 WHERE id = $2")
        .bind(encrypted)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace both halves of the server keypair (used only when neither the DB nor
/// `wg0.conf` had a key and a new one had to be minted).
pub async fn set_keypair(
    pool: &PgPool,
    id: Uuid,
    public_key: &str,
    encrypted: &[u8],
) -> sqlx::Result<()> {
    sqlx::query("UPDATE servers SET public_key = $1, private_key_encrypted = $2 WHERE id = $3")
        .bind(public_key)
        .bind(encrypted)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
