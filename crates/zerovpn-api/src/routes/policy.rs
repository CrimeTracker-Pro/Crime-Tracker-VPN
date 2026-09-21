use axum::{extract::{Path, State}, response::IntoResponse, Json};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    extractors::auth::RequireAdmin,
    state::AppState,
};
use zerovpn_db::repos::{audit, servers};

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct PolicyService {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub description: String,
    pub protocol: String,
    #[schema(value_type = String)]
    pub gateway_ip: IpNetwork,
    pub gateway_port: i32,
    #[schema(value_type = String)]
    pub backend_ip: IpNetwork,
    pub backend_port: i32,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ServiceBody {
    pub server_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub protocol: String,
    pub gateway_ip: String,
    pub gateway_port: i32,
    pub backend_ip: String,
    pub backend_port: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Deserialize, ToSchema)]
pub struct AssignmentBody {
    #[serde(default)] pub users: Vec<Uuid>,
    #[serde(default)] pub devices: Vec<Uuid>,
    #[serde(default)] pub groups: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct RuntimeStatus {
    pub server_id: Uuid,
    pub desired_generation: i64,
    pub applied_generation: i64,
    pub reconciler_mode: String,
    pub healthy: bool,
    pub drift_detected: bool,
    pub active_checksum: Option<String>,
    pub last_error: Option<String>,
    pub last_reconciled_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct PolicyRevision {
    pub id: Uuid,
    pub server_id: Uuid,
    pub revision_number: i64,
    pub checksum: String,
    pub status: String,
    pub apply_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub applied_at: Option<OffsetDateTime>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ModeBody { pub mode: String }

async fn bump_generation(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, server_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO vpn_policy_runtime (server_id, desired_generation) VALUES ($1, 1) ON CONFLICT (server_id) DO UPDATE SET desired_generation = vpn_policy_runtime.desired_generation + 1, updated_at = NOW()")
        .bind(server_id).execute(&mut **tx).await?;
    Ok(())
}

fn parse_host(value: &str, field: &str) -> ApiResult<IpNetwork> {
    let parsed: IpNetwork = value.parse().map_err(|_| ApiError::Validation(format!("{field} must be an IP address or CIDR")))?;
    Ok(parsed)
}

async fn validate_body(state: &AppState, body: &ServiceBody) -> ApiResult<(IpNetwork, IpNetwork)> {
    if body.name.trim().is_empty() || body.name.len() > 64 { return Err(ApiError::Validation("name must be 1..=64 characters".into())); }
    if body.protocol != "tcp" && body.protocol != "udp" { return Err(ApiError::Validation("protocol must be tcp or udp".into())); }
    if !(1..=65535).contains(&body.gateway_port) || !(1..=65535).contains(&body.backend_port) { return Err(ApiError::Validation("ports must be 1..=65535".into())); }
    let gateway = parse_host(&body.gateway_ip, "gateway_ip")?;
    let backend = parse_host(&body.backend_ip, "backend_ip")?;
    let server = servers::find_by_id(&state.pool, body.server_id).await?.ok_or(ApiError::NotFound)?;
    if !server.cidr.contains(gateway.ip()) { return Err(ApiError::Validation("gateway_ip must belong to the VPN server CIDR".into())); }
    if backend.ip().is_unspecified() || backend.ip().is_multicast() { return Err(ApiError::Validation("backend_ip cannot be unspecified or multicast".into())); }
    Ok((gateway, backend))
}

pub async fn list_services(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query_as::<_, PolicyService>("SELECT id,server_id,name,description,protocol::text AS protocol,gateway_ip,gateway_port,backend_ip,backend_port,enabled,created_at,updated_at FROM vpn_services ORDER BY name")
        .fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

pub async fn create_service(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Json(body): Json<ServiceBody>) -> ApiResult<impl IntoResponse> {
    let (gateway, backend) = validate_body(&state, &body).await?;
    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO vpn_services (id,server_id,name,description,protocol,gateway_ip,gateway_port,backend_ip,backend_port,enabled,created_by) VALUES ($1,$2,$3,$4,$5::vpn_transport_protocol,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(body.server_id).bind(body.name.trim()).bind(body.description.trim()).bind(&body.protocol).bind(gateway).bind(body.gateway_port).bind(backend).bind(body.backend_port).bind(body.enabled).bind(actor.id)
        .execute(&mut *tx).await?;
    bump_generation(&mut tx, body.server_id).await?;
    tx.commit().await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_service_created", target_type: Some("vpn_service"), target_id: Some(id), metadata: json!({"name": body.name}), ip: None }).await?;
    Ok((axum::http::StatusCode::CREATED, Json(json!({"id": id, "status": "ok"}))))
}

pub async fn delete_service(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid,)> = sqlx::query_as("DELETE FROM vpn_services WHERE id=$1 RETURNING server_id").bind(id).fetch_optional(&mut *tx).await?;
    let server_id = row.ok_or(ApiError::NotFound)?.0;
    bump_generation(&mut tx, server_id).await?;
    tx.commit().await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_service_deleted", target_type: Some("vpn_service"), target_id: Some(id), metadata: json!({}), ip: None }).await?;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn set_assignments(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Path(id): Path<Uuid>, Json(body): Json<AssignmentBody>) -> ApiResult<impl IntoResponse> {
    let mut tx = state.pool.begin().await?;
    let server_id: Uuid = sqlx::query_scalar("SELECT server_id FROM vpn_services WHERE id=$1").bind(id).fetch_optional(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM vpn_service_users WHERE service_id=$1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM vpn_service_devices WHERE service_id=$1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM vpn_service_groups WHERE service_id=$1").bind(id).execute(&mut *tx).await?;
    for value in &body.users { sqlx::query("INSERT INTO vpn_service_users VALUES ($1,$2)").bind(id).bind(value).execute(&mut *tx).await?; }
    for value in &body.devices { sqlx::query("INSERT INTO vpn_service_devices VALUES ($1,$2)").bind(id).bind(value).execute(&mut *tx).await?; }
    for value in &body.groups { sqlx::query("INSERT INTO vpn_service_groups VALUES ($1,$2)").bind(id).bind(value).execute(&mut *tx).await?; }
    bump_generation(&mut tx, server_id).await?;
    tx.commit().await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_service_assignments_updated", target_type: Some("vpn_service"), target_id: Some(id), metadata: json!({"users": body.users.len(), "devices": body.devices.len(), "groups": body.groups.len()}), ip: None }).await?;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn validate(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let conflicts: Vec<(String, i32, String, i64)> = sqlx::query_as("SELECT protocol::text,gateway_port,host(gateway_ip),COUNT(*) FROM vpn_services WHERE enabled GROUP BY protocol,gateway_port,gateway_ip HAVING COUNT(*)>1")
        .fetch_all(&state.pool).await?;
    let errors = conflicts.into_iter().map(|(p,port,ip,_)| format!("conflicting mapping: {p} {ip}:{port}")).collect::<Vec<_>>();
    let unassigned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vpn_services s WHERE enabled AND NOT EXISTS (SELECT 1 FROM vpn_service_users WHERE service_id=s.id) AND NOT EXISTS (SELECT 1 FROM vpn_service_devices WHERE service_id=s.id) AND NOT EXISTS (SELECT 1 FROM vpn_service_groups WHERE service_id=s.id)").fetch_one(&state.pool).await?;
    let warnings = if unassigned > 0 { vec![format!("{unassigned} enabled service(s) have no assignments and will be inaccessible")] } else { vec![] };
    Ok(Json(ValidationResult { valid: errors.is_empty(), errors, warnings }))
}

pub async fn request_apply(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query("UPDATE vpn_policy_runtime SET desired_generation=desired_generation+1, updated_at=NOW() RETURNING server_id").fetch_all(&state.pool).await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_policy_apply_requested", target_type: Some("vpn_policy"), target_id: None, metadata: json!({"servers": rows.len()}), ip: None }).await?;
    Ok(Json(json!({"status":"queued"})))
}

pub async fn status(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query_as::<_, RuntimeStatus>("SELECT server_id,desired_generation,applied_generation,reconciler_mode,healthy,drift_detected,active_checksum,last_error,last_reconciled_at FROM vpn_policy_runtime ORDER BY server_id")
        .fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

pub async fn revisions(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query_as::<_, PolicyRevision>("SELECT id,server_id,revision_number,checksum,status::text AS status,apply_error,created_at,applied_at FROM vpn_policy_revisions ORDER BY revision_number DESC LIMIT 100")
        .fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

pub async fn active_rules(State(state): State<AppState>, RequireAdmin(_): RequireAdmin) -> ApiResult<impl IntoResponse> {
    let row: Option<(Uuid, i64, String, String)> = sqlx::query_as("SELECT r.id,r.revision_number,r.checksum,r.compiled_ruleset FROM vpn_policy_runtime p JOIN vpn_policy_revisions r ON r.id=p.active_revision_id LIMIT 1")
        .fetch_optional(&state.pool).await?;
    Ok(Json(match row {
        Some((id, number, checksum, ruleset)) => json!({"revision_id":id,"revision_number":number,"checksum":checksum,"ruleset":ruleset}),
        None => json!({"revision_id":null,"ruleset":null}),
    }))
}

pub async fn set_mode(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Json(body): Json<ModeBody>) -> ApiResult<impl IntoResponse> {
    if body.mode != "shadow" && body.mode != "enforce" { return Err(ApiError::Validation("mode must be shadow or enforce".into())); }
    sqlx::query("UPDATE vpn_policy_runtime SET reconciler_mode=$1,desired_generation=desired_generation+1,updated_at=NOW()")
        .bind(&body.mode).execute(&state.pool).await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_policy_mode_changed", target_type: Some("vpn_policy"), target_id: None, metadata: json!({"mode":body.mode}), ip: None }).await?;
    Ok(Json(json!({"status":"queued"})))
}

pub async fn rollback(State(state): State<AppState>, RequireAdmin(actor): RequireAdmin, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let server_id: Uuid = sqlx::query_scalar("SELECT server_id FROM vpn_policy_revisions WHERE id=$1 AND status IN ('validated','applied','rolled_back')")
        .bind(id).fetch_optional(&state.pool).await?.ok_or(ApiError::NotFound)?;
    sqlx::query("UPDATE vpn_policy_runtime SET requested_revision_id=$2,desired_generation=desired_generation+1,updated_at=NOW() WHERE server_id=$1")
        .bind(server_id).bind(id).execute(&state.pool).await?;
    audit::record(&state.pool, audit::AuditEntry { actor_user_id: Some(actor.id), action: "admin.vpn_policy_rollback_requested", target_type: Some("vpn_policy_revision"), target_id: Some(id), metadata: json!({}), ip: None }).await?;
    Ok(Json(json!({"status":"queued"})))
}
