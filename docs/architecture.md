# Architecture (condensed)

This file is the reference that lives with the code.

## High-level

```
                            Internet
                               │
        ┌──────────────────────┼─────────────────────┐
        │ TCP 80/443           │ UDP 51820           │
        ▼                      ▼                     │
   ┌─────────┐           ┌─────────┐    ┌─────────┐  │
   │ traefik │           │   wg    │                 │
   │ (proxy) │           │(host-net│                 │
   └────┬────┘           │ NET_ADM)│                 │
        │                └────┬────┘                 │
        │ /api/* /ws/*        │                      │
        ▼                     │ wg show              │
   ┌─────────┐                │                      │
   │   api   │◀───ZMQ SUB─────┤                      │
   │ (Axum)  │  tcp://worker:5555                    │
   │  + WS   │                ▼                      │
   └────┬────┘          ┌──────────┐                 │
        │               │  worker  │ poller (1s)     │
        │               │ (tokio   │ rollups         │
        │               │ + ZMQ    │ retention       │
        │               │   PUB)   │ health          │
        │               └────┬─────┘                 │
        ▼                    │                       │
   ┌──────────┐         ┌────▼─────┐                 │
   │ frontend │         │    db    │                 │
   │ (nginx)  │         │(Postgres)│                 │
   │  React   │         └──────────┘                 │
   └──────────┘                                      │
```

> `wg` is not a separate box — the **api** is the WireGuard host (see
> *WireGuard runtime* below). Sessions and background jobs are backed by
> Postgres; there is no `redis` and no external job queue — periodic work runs
> on plain `tokio` intervals. The reverse proxy is Traefik. This stack does not
> run a DNS resolver or configure DNS in generated peer profiles.

## Process model

- **api** (binary `zerovpn-api`): HTTP + WebSocket, Axum 0.8. Reads/writes the DB, subscribes to ZMQ for live data, fans out to connected WS clients, brings up the WireGuard interface in its own container/netns (userspace boringtun via `wg-quick`, not linked into the binary), sends invitation email via `zerovpn-mail`, and serves the OpenAPI spec.
- **worker** (binary `zerovpn-worker`): runs the WG poller (~1 s by default, env-tunable), bandwidth aggregator (a plain `tokio::time::interval`, not a cron/queue), per-server health sampler, and retention purger, and binds the ZMQ PUB socket on `tcp://0.0.0.0:5555`. It does not send email.
- **cli** (binary `zerovpn-cli`): admin tool — migrate DB, bootstrap admin, rotate keys.

## Data flow: live stats → browser

1. Worker's `wg show dump` poll computes per-peer RX/TX deltas every ~1 s (env-tunable).
2. Each delta is encoded as MessagePack (`zerovpn-wire::Event::StatsDelta`) and published on ZMQ topic `stats.peer.<uuid>`.
3. The api process subscribes (`stats.*`, `events.*`) and converts received events into `tokio::sync::broadcast` messages keyed by user.
4. WebSocket handlers (one per browser tab) filter the broadcast to events relevant to the authenticated user, encode them into MessagePack frames, and send them over the WS connection.
5. The browser deserializes via `@msgpack/msgpack` (JS). A WASM decoder sharing the `zerovpn-wire` Rust types is a planned optimization, not yet shipped.
6. The topology graph and live-rate widgets consume those decoded events.

## Persistence

- PostgreSQL 18, single primary. Connection pooling via sqlx (16 max in api, 4 in worker).
- Time-series `bandwidth_samples` partitioned monthly with `RANGE` partitions. **Raw rows are purged after 30 days by default**; override with `ZEROVPN_SAMPLE_RETENTION_DAYS=N` (set `N=0` to keep them indefinitely). Aggregates (`bandwidth_aggregates`) are unbounded regardless. See [runbook.md → Stats pipeline & disk growth](runbook.md#stats-pipeline--disk-growth) for the storage math.
- Per-server time-series `server_samples` (migration 5) tracks total RX/TX, peer counts, and handshake-rate per poll tick. Same partitioning + retention story as `bandwidth_samples`, with its own knob `ZEROVPN_SERVER_SAMPLE_RETENTION_DAYS` (default 30 days; `0` = keep indefinitely).

## Logging & privacy

The "no-logs" posture was deliberately reversed — ZeroVPN now retains full
operational logs for admin visibility. See CHANGELOG → "Policy reversal —
full logging system" for the decision record. Concretely:

- Full client IPs and User-Agent strings are captured on auth flows (no longer prefix-truncated or hashed), alongside account events, per-device handshake metadata, and WG peer endpoints — all surfaced to admins.
- Flow metadata (`destination_ips`, sourced from conntrack) backs the Topology → Flows view.
- Still **not** captured anywhere in the stack: traffic content / payloads and DNS query contents.
- Each operational table is purged on its own window — 30 days by default for `bandwidth_samples`, `server_samples`, `destination_ips`, `audit_logs`, and `failed_logins` — overridable per table via the `ZEROVPN_*_RETENTION_DAYS` env vars (set a var to `0` to keep that table forever; e.g. `ZEROVPN_AUDIT_RETENTION_DAYS=0` for an unbounded audit trail). Soft-deleted users are hard-purged at +30 days.

## WireGuard runtime

The **api is the WireGuard host itself** — there is no separate `wg` container.
On boot the api:
1. materializes `wg0.conf` from the DB-stored server key (`servers.private_key_encrypted`, KEK-encrypted) onto ephemeral tmpfs — nothing WG-related persists on disk;
2. brings `wg0` up in its own container netns (`wg-quick` + **userspace boringtun**, in both dev and prod — no host kernel module) and applies forwarding/NAT best-effort;
3. re-adds every active peer (`reconcile_peers`) and thereafter programs peers on device create/revoke.

The **worker shares the api's netns** (`network_mode: service:api`) so its poller can `wg show wg0` for stats. Consequences:

- **Stateless api, DB-only recovery**: the api mounts no `wg_config` volume; a `pg_data` restore (+ the KEK) brings the whole tunnel back.
- **Trade-off**: the internet-facing api now runs privileged (`cap_add: NET_ADMIN` + `/dev/net/tun`) instead of the old `cap_drop: [ALL]` / `read_only` sidecar isolation. This was a deliberate choice to eliminate the api's volumes; the security-conservative alternative is a separate privileged WG sidecar.
- **No host dependency**: uses userspace boringtun (baked into the api image), so prod needs no kernel module / `modprobe` / `/lib/modules`.
- ⚠ **Verified on macOS/dev** (the same boringtun path). Validate the privileged prod image on a real Linux host before relying on it.

## Obfuscation

- Device `.conf` downloads are vanilla WireGuard, which the official WireGuard clients accept.
- wstunnel (escape-hatch transport for UDP-blocked networks) is deferred to v1.1.
