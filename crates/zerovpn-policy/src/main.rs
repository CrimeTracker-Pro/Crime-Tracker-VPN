use std::{collections::{BTreeMap, BTreeSet}, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use ipnetwork::IpNetwork;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{io::AsyncWriteExt, process::Command};
use tracing::{error, info};
use uuid::Uuid;

const FILTER_TABLE: &str = "crimetracker_vpn_filter";
const NAT_TABLE: &str = "crimetracker_vpn_nat";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Service {
    id: Uuid,
    name: String,
    protocol: String,
    gateway_ip: IpNetwork,
    gateway_port: i32,
    backend_ip: IpNetwork,
    backend_port: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct Runtime {
    server_id: Uuid,
    desired_generation: i64,
    applied_generation: i64,
    reconciler_mode: String,
    active_checksum: Option<String>,
    requested_revision_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct Snapshot<'a> {
    server_id: Uuid,
    generation: i64,
    services: &'a [Service],
    peers: &'a BTreeMap<Uuid, BTreeSet<String>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json().init();
    let database_url = std::env::var("ZEROVPN_DATABASE_URL").context("ZEROVPN_DATABASE_URL is required")?;
    let interval = std::env::var("ZEROVPN_POLICY_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(15);
    let pool = PgPoolOptions::new().max_connections(5).connect(&database_url).await?;
    info!(interval, "policy reconciler started");
    loop {
        if let Err(err) = reconcile_all(&pool).await { error!(?err, "policy reconcile failed"); }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

async fn reconcile_all(pool: &PgPool) -> Result<()> {
    let runtimes = sqlx::query_as::<_, Runtime>("SELECT server_id,desired_generation,applied_generation,reconciler_mode,active_checksum,requested_revision_id FROM vpn_policy_runtime ORDER BY server_id")
        .fetch_all(pool).await?;
    for runtime in runtimes {
        if let Err(err) = reconcile(pool, &runtime).await {
            sqlx::query("UPDATE vpn_policy_runtime SET healthy=FALSE,last_error=$2,last_reconciled_at=NOW(),updated_at=NOW() WHERE server_id=$1")
                .bind(runtime.server_id).bind(err.to_string()).execute(pool).await?;
            return Err(err);
        }
    }
    Ok(())
}

async fn reconcile(pool: &PgPool, runtime: &Runtime) -> Result<()> {
    if let Some(revision_id) = runtime.requested_revision_id {
        return apply_requested_revision(pool, runtime, revision_id).await;
    }
    let services = sqlx::query_as::<_, Service>("SELECT id,name,protocol::text AS protocol,gateway_ip,gateway_port,backend_ip,backend_port FROM vpn_services WHERE server_id=$1 AND enabled ORDER BY protocol,gateway_ip,gateway_port")
        .bind(runtime.server_id).fetch_all(pool).await?;
    let peer_rows: Vec<(Uuid, IpNetwork)> = sqlx::query_as(
        "WITH assigned AS ( \
           SELECT service_id,device_id FROM vpn_service_devices \
           UNION SELECT su.service_id,d.id FROM vpn_service_users su JOIN devices d ON d.user_id=su.user_id \
           UNION SELECT sg.service_id,gd.device_id FROM vpn_service_groups sg JOIN vpn_group_devices gd ON gd.group_id=sg.group_id \
           UNION SELECT sg.service_id,d.id FROM vpn_service_groups sg JOIN vpn_group_users gu ON gu.group_id=sg.group_id JOIN devices d ON d.user_id=gu.user_id \
         ) SELECT DISTINCT a.service_id,d.allocated_ip FROM assigned a JOIN devices d ON d.id=a.device_id JOIN users u ON u.id=d.user_id JOIN vpn_services s ON s.id=a.service_id \
         WHERE s.server_id=$1 AND s.enabled AND d.status='active' AND u.status='active'"
    ).bind(runtime.server_id).fetch_all(pool).await?;
    let mut peers: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for (service_id, ip) in peer_rows { peers.entry(service_id).or_default().insert(ip.ip().to_string()); }
    let rules = compile_rules(&services, &peers)?;
    let checksum = hex::encode(Sha256::digest(rules.as_bytes()));
    let drift = runtime.active_checksum.as_deref().is_some_and(|value| value != checksum);

    if runtime.active_checksum.is_some()
        && runtime.desired_generation == runtime.applied_generation
        && !drift
    {
        sqlx::query("UPDATE vpn_policy_runtime SET healthy=TRUE,last_error=NULL,last_reconciled_at=NOW(),updated_at=NOW() WHERE server_id=$1")
            .bind(runtime.server_id).execute(pool).await?;
        return Ok(());
    }

    validate_rules(&rules).await?;
    let snapshot = serde_json::to_value(Snapshot { server_id: runtime.server_id, generation: runtime.desired_generation, services: &services, peers: &peers })?;
    let revision_id = Uuid::now_v7();
    let revision_number: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(revision_number),0)+1 FROM vpn_policy_revisions WHERE server_id=$1")
        .bind(runtime.server_id).fetch_one(pool).await?;
    sqlx::query("INSERT INTO vpn_policy_revisions (id,server_id,revision_number,desired_snapshot,compiled_ruleset,checksum,status) VALUES ($1,$2,$3,$4,$5,$6,'validated')")
        .bind(revision_id).bind(runtime.server_id).bind(revision_number).bind(snapshot).bind(&rules).bind(&checksum).execute(pool).await?;

    if runtime.reconciler_mode == "enforce" {
        apply_rules(&rules).await?;
        sqlx::query("UPDATE vpn_policy_revisions SET status='applied',applied_at=NOW() WHERE id=$1").bind(revision_id).execute(pool).await?;
    } else {
        info!(server_id=%runtime.server_id, revision_number, "shadow policy validated; rules not applied");
    }
    sqlx::query("UPDATE vpn_policy_runtime SET applied_generation=$2,active_revision_id=$3,last_good_revision_id=CASE WHEN reconciler_mode='enforce' THEN $3 ELSE last_good_revision_id END,active_checksum=$4,healthy=TRUE,drift_detected=FALSE,last_error=NULL,last_reconciled_at=NOW(),updated_at=NOW() WHERE server_id=$1")
        .bind(runtime.server_id).bind(runtime.desired_generation).bind(revision_id).bind(checksum).execute(pool).await?;
    Ok(())
}

async fn apply_requested_revision(pool: &PgPool, runtime: &Runtime, revision_id: Uuid) -> Result<()> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT compiled_ruleset,checksum FROM vpn_policy_revisions WHERE id=$1 AND server_id=$2",
    ).bind(revision_id).bind(runtime.server_id).fetch_optional(pool).await?;
    let (rules, checksum) = row.context("requested policy revision does not exist")?;
    validate_rules(&rules).await?;
    if runtime.reconciler_mode == "enforce" {
        apply_rules(&rules).await?;
        sqlx::query("UPDATE vpn_policy_revisions SET status='rolled_back',applied_at=NOW() WHERE id=$1")
            .bind(revision_id).execute(pool).await?;
    }
    sqlx::query("UPDATE vpn_policy_runtime SET requested_revision_id=NULL,applied_generation=desired_generation,active_revision_id=$2,last_good_revision_id=CASE WHEN reconciler_mode='enforce' THEN $2 ELSE last_good_revision_id END,active_checksum=$3,healthy=TRUE,drift_detected=FALSE,last_error=NULL,last_reconciled_at=NOW(),updated_at=NOW() WHERE server_id=$1")
        .bind(runtime.server_id).bind(revision_id).bind(checksum).execute(pool).await?;
    info!(server_id=%runtime.server_id, %revision_id, mode=%runtime.reconciler_mode, "policy rollback processed");
    Ok(())
}

fn short_id(id: Uuid) -> String { id.simple().to_string()[..12].to_owned() }

fn compile_rules(services: &[Service], peers: &BTreeMap<Uuid, BTreeSet<String>>) -> Result<String> {
    let mut out = String::new();
    out.push_str(&format!("table inet {FILTER_TABLE} {{\n"));
    for service in services {
        let values = peers.get(&service.id).map(|v| v.iter().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default();
        out.push_str(&format!(" set svc_{} {{ type ipv4_addr; elements = {{ {} }} }}\n", short_id(service.id), values));
    }
    out.push_str(" chain input { type filter hook input priority filter; policy accept; ct state established,related accept; iifname \"wg0\" ip daddr 10.0.0.1 icmp type echo-request limit rate 10/second accept; iifname \"wg0\" drop; }\n");
    out.push_str(" chain forward { type filter hook forward priority filter; policy accept; ct state established,related accept;\n");
    for service in services {
        if !service.gateway_ip.ip().is_ipv4() || !service.backend_ip.ip().is_ipv4() { bail!("IPv6 gateway mappings are not supported yet"); }
        out.push_str(&format!("  iifname \"wg0\" ip saddr @svc_{} ip daddr {} {} dport {} counter accept comment \"service:{}\"\n", short_id(service.id), service.backend_ip.ip(), service.protocol, service.backend_port, service.id));
    }
    out.push_str("  iifname \"wg0\" counter drop\n }\n}\n");
    out.push_str(&format!("table ip {NAT_TABLE} {{\n chain prerouting {{ type nat hook prerouting priority dstnat; policy accept;\n"));
    for service in services {
        out.push_str(&format!("  iifname \"wg0\" ip daddr {} {} dport {} dnat ip to {}:{}\n", service.gateway_ip.ip(), service.protocol, service.gateway_port, service.backend_ip.ip(), service.backend_port));
    }
    out.push_str(" }\n chain postrouting { type nat hook postrouting priority srcnat; policy accept;\n");
    for service in services {
        out.push_str(&format!("  oifname \"eth0\" ip saddr 10.0.0.0/22 ip daddr {} {} dport {} masquerade\n", service.backend_ip.ip(), service.protocol, service.backend_port));
    }
    out.push_str(" }\n}\n");
    Ok(out)
}

async fn nft_input(args: &[&str], rules: &str) -> Result<()> {
    let mut child = Command::new("nft").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child.stdin.take().context("nft stdin")?.write_all(rules.as_bytes()).await?;
    let output = child.wait_with_output().await?;
    if !output.status.success() { bail!("nft failed: {}", String::from_utf8_lossy(&output.stderr)); }
    Ok(())
}

async fn prepare_batch(rules: &str) -> Result<String> {
    // The reconciler owns only these two tables. Deleting and recreating them
    // occurs in one nft transaction; Docker and host rules are untouched.
    let existing = Command::new("nft").args(["list", "table", "inet", FILTER_TABLE]).output().await?.status.success();
    let existing_nat = Command::new("nft").args(["list", "table", "ip", NAT_TABLE]).output().await?.status.success();
    let mut batch = String::new();
    if existing { batch.push_str(&format!("delete table inet {FILTER_TABLE}\n")); }
    if existing_nat { batch.push_str(&format!("delete table ip {NAT_TABLE}\n")); }
    batch.push_str(rules);
    Ok(batch)
}

async fn validate_rules(rules: &str) -> Result<()> {
    nft_input(&["--check", "-f", "-"], &prepare_batch(rules).await?).await
}

async fn apply_rules(rules: &str) -> Result<()> {
    nft_input(&["-f", "-"], &prepare_batch(rules).await?).await
}
