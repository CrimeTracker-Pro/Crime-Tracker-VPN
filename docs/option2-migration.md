# Option 2: host WireGuard migration

Status: production Option 2 cutover is running in verification as of
2026-09-24 22:49 UTC. The host tunnel is active; final client acceptance and
rollback-asset retention are still open. Historical preparation notes below
describe the state before cutover and must not be read as current status.

### Current production verification

The off-host backup and test device were confirmed by the operator. The
protected on-host backup is `/var/backups/crimetracker-vpn-option2.KleROcHJ`;
its PostgreSQL dump was restored successfully in a disposable PostgreSQL 18
container with 24 devices. Migrations 44 and 45 are applied. A transitional
legacy-tunnel worker established 24 durable peer checkpoints before the
legacy API and worker were stopped. The host agent was installed under
systemd, with the existing server key identity verified against both live
WireGuard and the database before cutover.

The pinned `option2-simple-20260925` API, worker, and frontend are running.
The host owns `wg0` at `10.0.0.1/22` and UDP 51820; its public key and all
24 peer public keys match the database. `host_agent_reconcile` is `applied`
with desired/applied revision 1 and matching digests. All 24 usage checkpoints
use one host interface generation. Worker and API restarts retained the host
tunnel, the applied revision, generation, and monotonically increasing device
totals. API `/health` and `/ready` pass; the frontend is healthy. Scoped VPN
forwarding/NAT chains and hooks are installed. A recent handshake exists, but
it does not substitute for the operator's real-client SMB/SSH, HTTPS, peer,
and blocked-LAN checks. Do not discard the legacy images or backup yet.

## What must be preserved

The host will own `wg0` at `10.0.0.1/22` and UDP 51820. PostgreSQL remains the
source of desired peers. The Ubuntu agent applies them and keeps a protected
last-known-good copy. The simpler deployment relies on Docker's bridge and
firewall being available when restoring `wg0` after a host reboot. If Docker
is unavailable at boot, the agent retries after Docker recovers; this is a
documented limitation, not an automatic Docker-independent recovery feature.

Preserve the server private/public key pair, every active peer public key and
allocated IP, any peer preshared key, keepalive, endpoint, client AllowedIPs,
and each device's recorded lifetime RX/TX and quota state. Preserve the
gateway-address behavior for SMB, SSH, HTTPS, and permitted peer traffic.

Updated access decision: every enabled VPN peer may reach **all services on
the Ubuntu host**, without per-device or per-service Host Access restrictions.
This includes SSH and any other listener bound to an address reachable from
`wg0`; verify this exposure before cutover. Existing peer-to-peer behavior,
LAN forwarding restrictions, internet routing, and the controlled
HTTPS/Traefik path remain unchanged. The old `vpn_services` data and rules
must stay intact until cutover/rollback is complete; removing the Host Access
feature from the new agent is not permission to delete production data now.

## Phase 1: read-only production inventory

Run these on the **actual deployment host**, not in a development container.
Keep outputs in a private incident/migration folder. WireGuard dumps and the
database backup contain secrets; do not paste them into issues or logs.

1. Record host/VM layout, Ubuntu release, kernel, WireGuard support, network
   interfaces, Docker version, firewall manager, router UDP forwarding, and
   which process owns UDP 51820. Confirm whether `192.168.1.20` is the Ubuntu
   host and which Traefik instance serves CrimeTracker.
2. Record `docker compose ps`, `docker compose config`, relevant image IDs,
   `ip -brief address`, `ip route`, `ss -lunp`, and the complete firewall
   ruleset using the host's active firewall tool. Record Samba configuration
   and listeners. Protect Compose output because it may include environment
   secrets.
3. Back up PostgreSQL with `pg_dump -Fc` and separately back up `.env`, the KEK,
   Compose, Samba, Traefik, and firewall configuration. Verify that a dump can
   be listed with `pg_restore --list`; perform a restore rehearsal on an
   isolated database before cutover. Keep a copy off the host.
4. Inside the current API container, capture the live WireGuard interface
   identity and peers. `wg show wg0 dump` includes the server private key and
   peer preshared keys: save it only in the protected backup. A sanitized
   comparison can use `wg show wg0 public-key`, `wg show wg0 allowed-ips`,
   `wg show wg0 persistent-keepalive`, and `wg show wg0 transfer`.
5. Query the database for the rows below. Export to a protected CSV or backup,
   recording the query time in UTC. Compare active database peers with the
   sanitized live peer list and resolve discrepancies before cutover.

```sql
SELECT id, name, endpoint_host, endpoint_port, public_key, cidr, mtu,
       persistent_keepalive, is_active,
       private_key_encrypted IS NOT NULL AS has_server_key
  FROM servers ORDER BY created_at;

SELECT id, user_id, server_id, name, public_key, allocated_ip, status,
       preshared_key_encrypted IS NOT NULL AS has_preshared_key,
       allowed_ips_override, lifetime_rx_bytes, lifetime_tx_bytes,
       current_month_bytes, monthly_byte_cap, auto_paused
  FROM devices ORDER BY server_id, allocated_ip;

SELECT id, monthly_byte_cap, current_month_bytes,
       month_baseline_bytes, month_baseline_at
  FROM users ORDER BY id;

SELECT s.id, s.name, s.protocol, s.gateway_ip, s.gateway_port,
       s.backend_ip, s.backend_port, s.enabled, s.allow_all_peers,
       COALESCE(array_agg(sd.device_id) FILTER
         (WHERE sd.device_id IS NOT NULL), '{}') AS permitted_devices
  FROM vpn_services s
  LEFT JOIN vpn_service_devices sd ON sd.service_id = s.id
 GROUP BY s.id ORDER BY s.gateway_port, s.name;
```

Record successful and denied connection tests from at least one ordinary VPN
device and one SSH-authorized device: tunnel handshake, gateway ping, SMB share
read/write, SSH, both CrimeTracker domains, and a blocked office-LAN target.
Record client and server timestamps for the intermittent latency/SMB issue.

Phase 1 passes only when the inventory, backup restore rehearsal, active-peer
comparison, and baseline access tests are available for review.

### Read-only snapshot from 2026-09-24 18:29 UTC

The deployment was reachable on the local Ubuntu host. `enp1s0` has
`192.168.1.20/24`; the kernel reports `7.0.0-31-generic`. Docker has the
VPN API, worker, and PostgreSQL 18 containers running. The API has
`CAP_NET_ADMIN`; the host listens on UDP 51820. The database reports migration
43 as its latest applied migration, so migration 44 has **not** been applied.

The active server row is `vpn.bha3.in:51820`, `10.0.0.0/22`, keepalive 30,
with an encrypted server key present. Its public key matches live `wg0`.
PostgreSQL has 24 active devices, and live `wg0` reports 24 peers. Sorted
public-key sets and public-key/allowed-IP sets match exactly. Device
lifetime totals summed across those rows were 6,603,652,420 device RX bytes
and 1,247,262,432 device TX bytes. These values are a dated observation, not
the eventual cutover baseline. No device has a stored preshared key in this
snapshot. The two enabled Host Access rules are SMB to `192.168.1.20:445`
for all peers and SSH to `192.168.1.20:22` for assigned devices; one device
assignment exists.

The production database, keys, firewall rules, client tests, and backup restore
have **not** been captured or changed as part of this snapshot. Repeat the
complete inventory and backup immediately before any production migration.

### Additional host binding check (2026-09-25)

The host currently has no `wg0`; it remains inside the VPN container. Host
Samba has `bind interfaces only = yes` with `interfaces = 127.0.0.1
192.168.1.20`, and its TCP 445/139 listeners use those addresses, not the
future `10.0.0.1`. Host SSH listens on `0.0.0.0:22`. A broad VPN-to-host INPUT
allow alone will therefore **not** preserve SMB access to `10.0.0.1:445`.
In staging, either bind Samba to the new VPN address/interface or provide a
specific gateway-to-local-address translation; validate the existing client
profile before cutover. Do not change Samba on the live host in this phase.
The firewall ruleset was unavailable during this initial read-only check;
the operator-supplied snapshot below resolves that inventory gap.

### Firewall snapshot supplied 2026-09-25 01:02 UTC

The supplied `nft list ruleset` and `iptables-save` show Docker using the
`iptables-nft` backend. The `ip nat`, `ip filter`, and `ip raw` chains shown
are Docker-managed: do **not** flush or replace these tables. Host IPv4 INPUT
currently has policy ACCEPT. Host IPv4 FORWARD has policy DROP and jumps to
`DOCKER-USER` before Docker's bridge rules. Host IPv4 forwarding is enabled.
No host `wg0` exists yet.

Docker currently DNATs host UDP 51820 to the API container at
`172.19.0.2:51820`. The host agent must refuse to create/activate host `wg0`
until the old API's `51820:51820/udp` publish has been removed and a fresh
read-only port/firewall snapshot confirms it is gone. Do not delete the DNAT
rule by hand: Docker owns it and will remove it with the old port mapping.

The existing FORWARD DROP would block host `wg0` peer-to-peer forwarding
unless a narrow `wg0` → `wg0` permit is installed at Docker's supported
`DOCKER-USER` insertion point. LAN and general internet forwarding must
remain denied. Host INPUT ACCEPT means no new per-device Host Access filter
is required for host-local listeners, but Samba's current binding still
prevents direct `10.0.0.1:445` access.

Docker also publishes TCP 80/443 to Traefik at `172.18.0.2`. The existing
CrimeTracker middleware accepts the `172.18.0.0/16` Traefik bridge source,
not VPN peer `10.0.0.0/22` addresses. A direct host-`wg0` path to published
443 may reach Traefik with a different source than the old container's
SNATed path and be rejected. Stage a scoped source-translation or an
equivalent reviewed Traefik policy change, and test both domains from a VPN
client before cutover. The current host route to `172.18.0.2` uses
`br-951bf515254e` with source `172.18.0.1`; discover and verify these
values again at cutover because Docker addresses can change. Do not widen
the middleware on the live stack now.

The new agent has a read-only `iptables-save` preflight parser. It intentionally
rejects the current snapshot because the old UDP publish remains. This is
only one gate: the final cutover must also verify the listener owner with
`ss`, current Compose port bindings, Samba, Traefik, and client behavior.
The offline forwarding plan now requires a matching route to Docker's
published HTTPS target and specifies only `wg0` peer-to-peer forwarding,
HTTPS to that target with the verified bridge source, and a default deny for
all other `wg0` forwarding. A command executor can stage dedicated VPN chains,
attach narrow jumps to `DOCKER-USER` and NAT `POSTROUTING`, and detach them on
failure. Its commands have passed both fake-runner and disposable-namespace
tests; no production activation path is deployed. If detaching a jump fails, it leaves
the referenced chain intact for operator recovery rather than flushing it.
A first-cutover backend now tests the ordering of firewall attachment,
WireGuard activation, verification, and rollback with fake host operations.
It requires an absent host `wg0` and rechecks the current network plan just
before activation. A separate backend handles updates to an existing `wg0`.
A concrete read-only host probe now collects `iptables-save`, the route to
Traefik, UDP listeners, and the presence of host `wg0`. Its first-cutover
preflight refuses an occupied UDP 51820, an existing VPN-owned chain, the old
Docker UDP publish, or a changed Docker bridge/HTTPS target. This probe has
not been exercised on the deployment host; the live host is unchanged.
The first-cutover command layer now has a system runner and an injectable
fake runner. It creates a kernel `wg0`, assigns `10.0.0.1/22`, passes the
secret native configuration to `wg setconf` over standard input, brings the
link up, and verifies identity, port, address, and exact enabled peer `/32`
set. It deletes only an interface this process successfully created. The
system runner is now selected by a privileged agent apply operation, but the
service is **not deployed** and it has not been exercised on the deployment
host. The coordinator now takes an exclusive
owner-only apply lock across the entire transaction, and system commands and
read-only observations have bounded waits. Recovery after interruption and
broader isolated/staging integration tests are still required before a
production cutover.
An opt-in test now runs the first cutover with real `ip`, `wg`, and `iptables`
commands inside a disposable Docker network namespace. It verified activation,
identity/peer checks, rule cleanup, and rollback after an injected link-up
failure. A fresh read-only reconciler verifies the protected applied journal,
active `wg0` identity/port/address/exact peer IPs, current Docker route, and
the exact VPN-owned firewall rules; tampered state is rejected. A fresh
first-cutover backend still refuses to take over an already-active `wg0`;
an explicit restore coordinator can rebuild a missing tunnel from the
protected journal without advancing its revision, and rolls back a failed
restore. This was also exercised with real commands in the disposable
namespace. Startup verification/restore and the privileged apply socket are
now wired into the agent binary, but not installed as a host service.
The isolated test now also updates an existing `wg0` with `wg syncconf`,
checks the new peer keepalive and preshared-key settings, and restores the
old secret-bearing `wg showconf` snapshot. The update backend refuses to
start unless the active tunnel and firewall first match the durable journal;
it is exposed only through the privileged agent apply socket, not the API.
This test does not prove the production Docker/Traefik/Samba topology or
authorize deployment. Run it only with a private container network namespace,
never with Docker `--network host`.

## Usage continuity: required before cutover

The existing `devices.lifetime_rx_bytes` and `lifetime_tx_bytes` are the
authoritative device totals. Worker RX/TX uses the **device perspective**:
device RX equals server WireGuard TX; device TX equals server WireGuard RX.
Keep that convention in every new API and migration query.

The pre-migration poller stores its previous raw counters only in memory. On
first observation after a worker restart it calls `seed_lifetime` with
`GREATEST`. That avoids an obvious double count but does not reliably recover
bytes if the interface counter reset while the worker was down. Migration 44
and the updated worker add a durable peer checkpoint. The current sidebar Net
I/O is still Docker's raw cumulative container total, which can reset on
recreation. `wg0 Real I/O` is a live rate, not a lifetime counter.

The checkpoint records device ID, public key, source generation, raw server
RX/TX, and last sample time. The device row carries its accounted lifetime
RX/TX. Each accepted advance commits the checkpoint, lifetime increment,
monthly usage, and bandwidth sample in **one database transaction**. Replaying
the same sample changes none of them. The temporary legacy source detects
counter regressions; the host agent must provide an explicit interface
generation that survives agent process restarts while `wg0` remains the same,
and changes when `wg0` is recreated. That closes the reset-during-worker-outage
case which raw counter comparison alone cannot always identify.

Deploy migration 44 **before** deploying the updated worker. The current
`make up-prod` starts the worker without running migrations first. For this
accounting-only stage, build the new images without starting them, stop the old
worker, run `make migrate` with the newly built CLI image, and recreate only
the worker in the existing API network namespace. Do not recreate the API at
this stage: it still owns the live tunnel. Record the old worker's final poll
and the new worker's first checkpoint. Traffic between them can be absent from
the lifetime totals because there is no durable old raw checkpoint to bridge
that one-time gap. Never run old and new workers at the same time.

At planned cutover, stop new peer traffic, take a final old-interface sample,
wait for the worker's database commit, and record a cutover marker with the
final raw counters and lifetime totals. Then start host `wg0` as a **new
generation**. Its first observed counters are accounted only once according
to the new checkpoint rule. Never seed the new raw counter by replacing the
existing lifetime total. Monthly quota baselines remain unchanged.

The updated sidebar labels this value **VPN total** and reads the durable
`server_vpn_usage` totals. Migration 44 seeds them from existing device
lifetimes; subsequent peer deltas update them in the same transaction as the
device counters. These are summed WireGuard peer counters in the server
perspective, not Docker Net I/O or raw `wg0` interface packet counters. The
old Docker figure and this new VPN total must not be compared as if they were
the same measurement. Keep live `wg0 Real I/O` as a rate.

The maximum unrecorded traffic after an abrupt loss of `wg0` is the traffic
since its last durable sample. A planned shutdown must sample before deleting
the interface. Document and monitor any gap after an unexpected host failure.

## Delivery gates

1. Agent protocol and non-production agent: authenticated Unix socket,
   validated whole-state revision, health/stats, protected last-known-good
   state, audit, and rejection of stale revisions.
2. Host data plane: kernel `wg0`, host INPUT access for all enabled VPN peers,
   a narrow peer-forward rule through `DOCKER-USER`, existing restricted
   forwarding/NAT behavior, controlled HTTPS/Traefik path, and no unrelated
   firewall table changes.
3. API and database: desired/applied revision tracking, device/quota write
   paths reconciled through the agent, Host Access UI/API retirement only
   after the old gateway is no longer needed, visible pending/failure
   state, and durable usage checkpoints.
4. Worker and Compose: agent statistics, normal worker Docker network, internal
   ZMQ, removal of BoringTun and container networking privileges, and correct
   server health labels.
5. Staging validation: existing-style client profile, peer changes, restart
   and outage scenarios, traffic totals, quota, SMB and SSH from ordinary
   peers, CrimeTracker, and still-denied LAN/internet paths.
6. Scheduled production cutover: backup and final counters, stop old gateway,
   start host gateway with the **same identity**, validate all service and
   accounting gates, then observe before removing rollback assets.

### Operator cutover sequence (historical plan; executed through verification)

1. Build the four Option 2 artifacts with distinct candidate tags. The
   Option 2 Compose overlay pins `option2-simple-20260925` for API, worker,
   and frontend, leaving legacy `latest` untouched for rollback. Stage
   the host-agent executable from `deploy/Dockerfile.host-agent-artifact` and
   the candidate systemd unit; install neither by running it inside Docker.
2. Provision distinct owner-only read/apply token files and the server private
   key in `/etc/crimetracker-vpn`. Derive its public key locally and compare it
   with the active `servers.public_key` and the legacy live interface. Never
   print or paste the private key or tokens. Verify a database restore and
   record the current image digests and complete host firewall snapshot.
3. Migrate 44 and 45 before starting the new worker. Quiesce peer writes and
   confirm no client traffic. Record the final old worker poll, raw peer
   counters, durable device/server totals, and timestamp. Stop the legacy
   worker and API so Docker removes the container UDP 51820 publish. Do not
   delete the old images, Compose file, or Host Access rows.
4. Recheck host `wg0` absence, UDP 51820 ownership, Docker's HTTPS target and
   route, iptables/nft rules, Samba/SSH listeners, and the current backup.
   Start the systemd agent and require `state_status` to show no applied state.
   Activate the Option 2 Compose overlays using only the candidate images.
   The API's first reconciliation requests the guarded host first apply.
5. Require the admin host-agent status to become `applied` with equal desired
   and applied revision, the host `wg0` identity/port/peer IPs to match the
   inventory, and the worker's first host-generation checkpoint to commit.
   Exercise a real client for handshake, host SMB/SSH, CrimeTracker HTTPS,
   permitted peer traffic, and still-denied LAN/internet paths. Confirm totals
   increase only by observed deltas and remain stable across API/worker
   restarts. Monitor failures and usage before closing the maintenance window.
6. If any gate fails, stop the Option 2 API/worker first. Stop the host agent,
   remove **only** its recorded `wg0` and owned firewall chains after verifying
   their identity, then restore the retained legacy images/Compose deployment.
   Restore the database only if required and only from the verified backup;
   account separately for traffic during the host trial. Never run host and
   container gateways concurrently on UDP 51820.

The existing `make up-prod` / `scripts/deploy-prod.sh` still assume the legacy
shared network namespace and must **not** be used for Option 2 deployment.

### Phase 2 progress: non-production agent foundation

The new `zerovpn-host-agent` crate provides a versioned JSON-line Unix-socket
protocol with token authentication and `health`/`peer_stats` requests. It
queries the fixed `wg0` interface using `wg show wg0 dump`. The parser discards
the interface private key and peer pre-shared keys; only public peer metadata
and server-perspective traffic counters are returned. Request/response frames
are size-bounded, and request handling has a timeout. The token comes from a
file with owner-only permissions; the socket is created with mode `0660`.

The version-2 `validate_state` request accepts a complete candidate with a
monotonically increasing revision, server public key, peer keys and VPN IPs.
It no longer contains Host Access restrictions. It rejects duplicate or
invalid keys/IPs, stale revisions, and a change of server identity from the
saved state. It computes a state digest but
**does not apply or persist the candidate**. Validation outcomes are appended
to the audit log without storing the candidate's peer data. A protected state
journal can persist a verified applied state with an atomic replacement;
only a verified host apply may call that journal operation.
Corrupt saved state blocks agent startup rather than resetting its revision.

The apply coordinator now models snapshot → stage → activate → verify →
durable journal commit. A mock backend tests successful apply, stale replay,
rollback after failures at each stage, and rollback failure reporting. Real
first-cutover and existing-interface update backends are tested only in an
isolated namespace. A privileged `apply_state` socket operation now selects
the appropriate backend and requires a separate owner-only apply token; the
read/stats token cannot apply changes, and apply is disabled without its token
file. Candidate Compose overlays and a systemd unit now exist, but neither is
deployed. Before deployment, exercise uncertain-commit recovery and run the
API/worker/agent staging topology.
Cross-process apply locking now exists. A durable
interface-generation record now compares Linux boot ID,
network namespace, `wg0` ifindex, and server public key; it survives agent
process restarts and rotates on a different observed interface. Stats carry
this generation only after an applied state has established the expected
server identity. A sample is rejected if the interface changes while `wg`
is queried. The worker now has an **opt-in** host-agent stats source selected by
`ZEROVPN_WORKER__WG_STATS_SOURCE=host_agent`, with an absolute socket path and
protected read-token file. It rejects agent snapshots without a verified
interface generation and commits that generation with each peer's raw counters;
it never falls back to local `wg` in host mode. The legacy source remains the
default; the opt-in Compose overlay enables host mode only when selected at
cutover. In host mode, live `wg0 Real
I/O` also reads host-interface counters from the agent and resets its rate
baseline after a missed sample or interface-generation change. A fake socket test checks the
host-source exchange, but a running worker/DB/agent staging test is still
required. The live host-agent lifecycle still needs to prove exclusive ownership of `wg0`
before this generation can be trusted in production. The legacy gateway
remains active and unchanged.

The agent `state_status` response reports its applied journal revision and
digest even before `wg0` exists. Migration 45 adds durable desired/applied
revision, digest, and failure status. The API's opt-in `host_agent` controller
serializes whole-state snapshots through a PostgreSQL advisory transaction
lock, retries drift every five seconds, and advances past an uncertain agent
commit. Migration 45 gives peer-affecting device/user/server writes the same
lock before they modify rows, preventing a write from committing during an
agent apply. The admin-only `/admin/host-agent-status` endpoint exposes the
non-secret status. The legacy controller remains the default. Key rotation is
blocked in host-agent mode; it requires a separately planned identity change.
Stored peer preshared keys are decrypted from the database with the existing
KEK for each candidate state; a decryption or key-validation failure blocks
apply rather than silently clearing a PSK.
Host Access routes reject changes in host-agent mode, and the Option 2 frontend
build hides that page. Historical Host Access data is retained for rollback.

The Option 2 Compose overlays remove the API's UDP 51820 publish, shared
network namespace, NET_ADMIN, tunnel device, and worker NET_ADMIN. The worker
gets its own backend network endpoint and uses the host-agent read token. The
API gets the separate apply token. Host-specific Dockerfiles omit BoringTun and
local WireGuard tools; `deploy/zerovpn-host-agent.service` is a candidate
systemd unit, not an installed service. Validate the merged configuration with
`docker compose -f docker-compose.yml -f docker-compose.build.yml -f
docker-compose.option2-build.yml -f docker-compose.option2.yml config --quiet`
after setting `VPN_AGENT_READ_TOKEN_FILE` and `VPN_AGENT_APPLY_TOKEN_FILE`.
All 45 migrations applied to a disposable PostgreSQL 18 instance; migration
45 created eight desired-state serialization triggers. A separate API test
against disposable PostgreSQL and a fake Unix agent covered initial apply
with an encrypted PSK, pause, restart/no-op, failed-apply status, and retry.
The usage-checkpoint integration test passed against its own temporary
PostgreSQL instance. Real first-cutover/update/rollback tests passed inside a
disposable Docker network namespace with NET_ADMIN, never on the host.

Before the scheduled cutover, the production host ran the legacy API-owned
tunnel and noninteractive host administrator access was unavailable in this
workspace. The operator subsequently provisioned the key and tokens and
installed the systemd unit. A real-client acceptance test is still required;
the isolated namespace test does not substitute for it.
At 2026-09-24 21:33 UTC, the running legacy API reported 24 configured peers
and **one handshake within the previous five minutes**. Do not assume the
tunnel is idle from the earlier statement that nobody was using it; recheck
peer activity immediately before any outage window.

The draft host INPUT compiler produces a deterministic list of enabled VPN
peer IPs with no local port restrictions; disabled peers are excluded. This
is **intent only**, not an installed rule. The actual backend must allow that
local host traffic while preserving the separate FORWARD/NAT restrictions,
peer-to-peer handling, and HTTPS/Traefik route. Confirm Samba and SSH listen
on the expected host addresses so connections to `10.0.0.1` still work.

An offline renderer now creates the native `wg` server configuration for the
fixed UDP port 51820, including enabled peers, `/32` allowed IPs, optional
preshared keys, and keepalive values. The server private key is supplied from
a separate protected source, never from the desired-state socket request;
the renderer's output is redacted in debug logging and cleared on drop.
The first-cutover and update backends verify that this private key derives
to the desired server public key and pass the secret config through standard
input without an on-disk temporary file. `wg syncconf` and rollback have
passed in an isolated namespace; no production deployment uses them yet.
The server key is now loaded from an owner-only regular file in an
owner-controlled directory, rejecting symlinks and weak permissions. Startup
refuses an unknown active host `wg0`; with a saved journal it verifies an
active tunnel or restores a missing one only after the current Docker/route
preflight passes. The isolated test covers missing-key refusal, restore,
second-start verification, first apply, peer update, and stale revision
rejection. The service unit is a candidate artifact. Privileged token
provisioning, installation, and end-to-end production topology checks remain
pending.
The actual agent binary has also passed an opt-in Unix-socket test inside a
disposable network namespace: read-token health works, a read token cannot
apply, and the separate apply token advances the durable revision and live
WireGuard peer configuration. The apply endpoint is disabled by default and
has not been deployed on the production host.

The rollback artifact is the old container image/Compose configuration plus
the database, firewall, key, Samba, and Traefik backups. Rollback stops host
`wg0` before restarting the container gateway, so they never compete for
UDP 51820 or `10.0.0.1`. Compare post-rollback lifetime totals with the
cutover snapshot and account for any traffic passed during the host trial.

### Simple recovery boundary

The host agent keeps the last applied peer state, but restoring a missing
interface still requires Docker's current bridge route and firewall hooks.
Do not claim Docker-independent boot. Keep the legacy images and verified
backup for rollback, and verify a real VPN client after the scheduled cutover.

### Post-cutover SMB check

After the host cutover, Samba validated with `testparm` but did not listen on
the WireGuard address while `bind interfaces only = yes`. A temporary `wg0`
TCP/445 redirect to the LAN listener proved that the VPN and Samba credentials
worked. The operator then set `bind interfaces only = no`, confirmed a
`0.0.0.0:445` listener, removed the redirect, and opened a share directly at
`smb://10.0.0.1` from a remote Mac. The resulting config keeps `hosts allow`
for `10.0.0.0/22` and requires a Samba login. The final configuration delta
is in `deploy/samba-option2.patch`; the pre-change host file is backed up at
`/var/backups/crimetracker-vpn-option2.KleROcHJ/smb-before-wildcard.conf`.
This direct-client check does not yet prove recovery after a host reboot.
