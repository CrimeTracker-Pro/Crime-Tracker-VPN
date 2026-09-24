use axum::{extract::{Path, State}, Json};
use ipnetwork::IpNetwork;
use std::net::Ipv4Addr;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use zerovpn_db::repos::audit;

use crate::{
    bootstrap,
    error::{ApiError, ApiResult},
    extractors::auth::RequireAdmin,
    state::AppState,
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HostAccessRule {
    id: Uuid,
    name: String,
    description: String,
    protocol: String,
    gateway_ip: IpNetwork,
    gateway_port: i32,
    backend_ip: IpNetwork,
    backend_port: i32,
    enabled: bool,
    allow_all_peers: bool,
    device_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HostAccessDevice {
    id: Uuid,
    name: String,
    allocated_ip: IpNetwork,
    status: String,
    owner_email: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveHostAccessRule {
    name: String,
    #[serde(default)]
    description: String,
    protocol: String,
    backend_ip: String,
    gateway_port: i32,
    backend_port: i32,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    allow_all_peers: bool,
    #[serde(default)]
    device_ids: Vec<Uuid>,
}

fn default_true() -> bool { true }

fn require_legacy_gateway() -> ApiResult<()> {
    if std::env::var("ZEROVPN_WG__BACKEND").as_deref() == Ok("host_agent") {
        return Err(ApiError::Conflict(
            "Host Access rules are retired in host-agent mode; VPN peers can access host services".into(),
        ));
    }
    Ok(())
}

fn host_ip(value: &str) -> ApiResult<IpNetwork> {
    let ip = value.trim().parse::<Ipv4Addr>()
        .map_err(|_| ApiError::Validation("managed host must be a valid IPv4 address".into()))?;
    format!("{ip}/32").parse::<IpNetwork>()
        .map_err(|_| ApiError::Validation("managed host must be a valid IPv4 address".into()))
}

fn gateway_ip(cidr: IpNetwork) -> ApiResult<IpNetwork> {
    match cidr {
        IpNetwork::V4(network) if network.prefix() <= 30 => {
            let first = Ipv4Addr::from(u32::from(network.network()) + 1);
            format!("{first}/32").parse::<IpNetwork>()
                .map_err(|_| ApiError::Validation("server CIDR has no usable gateway address".into()))
        }
        _ => Err(ApiError::Validation("server CIDR must provide an IPv4 gateway".into())),
    }
}

fn validate(body: &SaveHostAccessRule) -> ApiResult<()> {
    if body.name.trim().is_empty() || body.name.len() > 80 {
        return Err(ApiError::Validation("name must contain 1–80 characters".into()));
    }
    if !matches!(body.protocol.as_str(), "tcp" | "udp") {
        return Err(ApiError::Validation("protocol must be tcp or udp".into()));
    }
    host_ip(&body.backend_ip)?;
    if !(1..=65535).contains(&body.gateway_port) || !(1..=65535).contains(&body.backend_port) {
        return Err(ApiError::Validation("ports must be between 1 and 65535".into()));
    }
    if !body.allow_all_peers && body.device_ids.is_empty() {
        return Err(ApiError::Validation("select at least one device or allow all peers".into()));
    }
    Ok(())
}

async fn reconcile(state: &AppState) {
    let iface = std::env::var("ZEROVPN_WG__INTERFACE").unwrap_or_else(|_| "wg0".into());
    bootstrap::ensure_peer_only_forwarding(&state.pool, &iface).await;
}

async fn list_rows(state: &AppState) -> ApiResult<Vec<HostAccessRule>> {
    Ok(sqlx::query_as(
        r#"SELECT s.id, s.name, s.description, s.protocol::text AS protocol,
                  s.gateway_ip, s.gateway_port, s.backend_ip, s.backend_port,
                  s.enabled, s.allow_all_peers,
                  COALESCE(array_agg(sd.device_id) FILTER (WHERE sd.device_id IS NOT NULL), '{}') AS device_ids
             FROM vpn_services s
        LEFT JOIN vpn_service_devices sd ON sd.service_id = s.id
         GROUP BY s.id
         ORDER BY s.gateway_port, s.name"#,
    ).fetch_all(&state.pool).await?)
}

pub async fn list(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<Json<Vec<HostAccessRule>>> {
    require_legacy_gateway()?;
    Ok(Json(list_rows(&state).await?))
}

pub async fn devices(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<Json<Vec<HostAccessDevice>>> {
    require_legacy_gateway()?;
    let rows = sqlx::query_as(
        r#"SELECT d.id,
                  d.name,
                  d.allocated_ip, d.status::text AS status, u.email AS owner_email
             FROM devices d JOIN users u ON u.id = d.user_id
            WHERE d.status <> 'revoked'
         ORDER BY d.allocated_ip"#,
    ).fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

pub async fn create(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Json(body): Json<SaveHostAccessRule>) -> ApiResult<Json<HostAccessRule>> {
    require_legacy_gateway()?;
    validate(&body)?;
    let (server_id, server_cidr): (Uuid, IpNetwork) = sqlx::query_as("SELECT id, cidr FROM servers WHERE is_active ORDER BY created_at LIMIT 1")
        .fetch_one(&state.pool).await?;
    let gateway_ip = gateway_ip(server_cidr)?;
    let backend_ip = host_ip(&body.backend_ip)?;
    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        r#"INSERT INTO vpn_services
           (id, server_id, name, description, protocol, gateway_ip, gateway_port,
            backend_ip, backend_port, enabled, allow_all_peers, created_by)
           VALUES ($1,$2,$3,$4,$5::vpn_transport_protocol,$6,$7,
                   $8,$9,$10,$11,$12)"#,
    ).bind(id).bind(server_id).bind(body.name.trim()).bind(body.description.trim())
      .bind(&body.protocol).bind(gateway_ip).bind(body.gateway_port).bind(backend_ip)
      .bind(body.backend_port).bind(body.enabled).bind(body.allow_all_peers).bind(actor.id).execute(&mut *tx).await?;
    for device_id in &body.device_ids {
        sqlx::query("INSERT INTO vpn_service_devices (service_id, device_id) SELECT $1, id FROM devices WHERE id = $2 AND server_id = $3")
            .bind(id).bind(device_id).bind(server_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.host_access_created", target_type: Some("host_access_rule"), target_id: Some(id), metadata: json!({"name": body.name}), ip: None }).await?;
    reconcile(&state).await;
    let row = list_rows(&state).await?.into_iter().find(|r| r.id == id).ok_or(ApiError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Path(id): Path<Uuid>, Json(body): Json<SaveHostAccessRule>) -> ApiResult<Json<HostAccessRule>> {
    require_legacy_gateway()?;
    validate(&body)?;
    let server_cidr: IpNetwork = sqlx::query_scalar("SELECT s.cidr FROM vpn_services v JOIN servers s ON s.id=v.server_id WHERE v.id=$1")
        .bind(id).fetch_one(&state.pool).await?;
    let gateway_ip = gateway_ip(server_cidr)?;
    let backend_ip = host_ip(&body.backend_ip)?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query(
        r#"UPDATE vpn_services SET name=$2, description=$3,
                  protocol=$4::vpn_transport_protocol, gateway_ip=$5, gateway_port=$6,
                  backend_ip=$7, backend_port=$8, enabled=$9, allow_all_peers=$10, updated_at=NOW()
            WHERE id=$1"#,
    ).bind(id).bind(body.name.trim()).bind(body.description.trim()).bind(&body.protocol)
      .bind(gateway_ip).bind(body.gateway_port).bind(backend_ip).bind(body.backend_port)
      .bind(body.enabled).bind(body.allow_all_peers).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 { return Err(ApiError::NotFound); }
    sqlx::query("DELETE FROM vpn_service_devices WHERE service_id=$1").bind(id).execute(&mut *tx).await?;
    for device_id in &body.device_ids {
        sqlx::query("INSERT INTO vpn_service_devices (service_id, device_id) SELECT $1, d.id FROM devices d JOIN vpn_services s ON s.id=$1 WHERE d.id=$2 AND d.server_id=s.server_id")
            .bind(id).bind(device_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.host_access_updated", target_type: Some("host_access_rule"), target_id: Some(id), metadata: json!({"name": body.name}), ip: None }).await?;
    reconcile(&state).await;
    let row = list_rows(&state).await?.into_iter().find(|r| r.id == id).ok_or(ApiError::NotFound)?;
    Ok(Json(row))
}

pub async fn delete(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Path(id): Path<Uuid>) -> ApiResult<Json<serde_json::Value>> {
    require_legacy_gateway()?;
    let changed = sqlx::query("DELETE FROM vpn_services WHERE id=$1").bind(id).execute(&state.pool).await?;
    if changed.rows_affected() == 0 { return Err(ApiError::NotFound); }
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.host_access_deleted", target_type: Some("host_access_rule"), target_id: Some(id), metadata: json!({}), ip: None }).await?;
    reconcile(&state).await;
    Ok(Json(json!({"status":"ok"})))
}
