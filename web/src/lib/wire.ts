/**
 * Wire schema mirroring crates/zerovpn-wire::Event.
 * Keep in sync; future iteration will replace this with WASM-decoded types
 * from the same Rust crate.
 */

export type Event =
  | { type: "heartbeat"; ts_ms: number }
  | {
      type: "stats_delta"
      device_id: string
      user_id: string
      rx_bytes: number
      tx_bytes: number
      rate_rx_bps: number
      rate_tx_bps: number
      /** Authoritative cumulative lifetime totals after this tick — the
       *  device's "Total RX/TX". Grows in real time and matches the server. */
      total_rx_bytes: number
      total_tx_bytes: number
      ts_ms: number
    }
  | {
      type: "handshake_change"
      device_id: string
      user_id: string
      last_handshake_ms: number
    }
  | {
      type: "peer_status_changed"
      device_id: string
      user_id: string
      status: "active" | "paused" | "revoked"
    }
  | {
      type: "server_health"
      server_id: string
      cpu_pct: number
      mem_used_bytes: number
      mem_total_bytes: number
      active_peers: number
      /** wg0 tunnel: bytes received per second (Real I/O row in the
       *  sidebar). Diff of cumulative counters since the previous tick. */
      wg_rx_bps: number
      /** wg0 tunnel: bytes transmitted per second. */
      wg_tx_bps: number
      /** Container Net I/O — **cumulative bytes** since container start,
       *  summed across every interface. Matches the "Net I/O" column from
       *  `docker stats <name>` exactly. Sidebar renders as `↓ 39.4 MB ·
       *  ↑ 13.1 MB`, no per-second conversion. */
      net_rx_total_bytes: number
      net_tx_total_bytes: number
      uptime_sec: number
      ts_ms: number
    }
  | {
      type: "server_sample"
      server_id: string
      total_rx_bytes: number
      total_tx_bytes: number
      rate_rx_bps: number
      rate_tx_bps: number
      peer_count: number
      online_count: number
      handshake_count: number
      ts_ms: number
    }
  | {
      /** A persisted-data mutation happened (emitted by the API, not the
       *  worker). Lets every other session of the same user — and any admin
       *  watching — invalidate the right query and reflect add/edit/delete in
       *  real time. `user_id` is null for admin-global resources (server,
       *  maintenance); `id` is null for bulk ops (reorder). */
      type: "data_changed"
      user_id: string | null
      resource: "device" | "user" | "server" | "maintenance"
      id: string | null
      action:
        | "created"
        | "updated"
        | "deleted"
        | "paused"
        | "unpaused"
        | "keys_rotated"
        | "reordered"
        | "connected"
    }
  | {
      /** Server-composed, ready-to-display notification. The client renders it
       *  as a toast and (when the tab is hidden + opted in) an OS notification.
       *  Covers connectivity / quota / security alerts without a per-category
       *  wire variant. */
      type: "notify"
      user_id: string | null
      level: "info" | "success" | "warning" | "error"
      title: string
      body: string | null
      url: string | null
      tag: string | null
    }
