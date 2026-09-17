import { create } from "zustand"

import type { Event } from "@/lib/wire"
import { formatTime } from "@/lib/datetime"
import { formatBps } from "@/lib/units"

const TAIL_CAP = 60

export interface TailLine {
  id: number
  tsMs: number
  kind: Event["type"]
  tone: "ok" | "warn" | "err" | "info" | "muted"
  text: string
  deviceId?: string
}

interface EventTailState {
  lines: TailLine[]
  connectedAt: number | null
  push(event: Event): void
  markConnected(): void
  reset(): void
}

let nextId = 1

function toLine(event: Event): TailLine | null {
  switch (event.type) {
    case "stats_delta":
      // Per-tick per-device throughput is intentionally **not** surfaced
      // in the tail — at 1 Hz × N peers it floods the log with redundant
      // information that already lives on the chart. The per-server
      // `server_sample` (one row/sec total) is the right granularity
      // for the tail. Status changes / handshakes still pass
      // through below.
      return null
    case "peer_status_changed":
      return {
        id: nextId++,
        tsMs: Date.now(),
        kind: event.type,
        tone: event.status === "active" ? "ok" : event.status === "paused" ? "warn" : "err",
        deviceId: event.device_id,
        text: `device.status · ${event.device_id.slice(0, 8)} → ${event.status}`,
      }
    case "handshake_change":
      return {
        id: nextId++,
        tsMs: event.last_handshake_ms,
        kind: event.type,
        tone: "info",
        deviceId: event.device_id,
        text: `device.handshake · ${event.device_id.slice(0, 8)} · ${formatTime(event.last_handshake_ms)}`,
      }
    case "server_health":
      return {
        id: nextId++,
        tsMs: event.ts_ms,
        kind: event.type,
        tone: event.cpu_pct > 80 ? "warn" : "muted",
        text: `server.health · ${event.server_id} · cpu ${event.cpu_pct.toFixed(0)}% · ${event.active_peers} peers`,
      }
    case "server_sample":
      // Per-server tick — admin-scoped. Surface when it has interesting
      // load, otherwise treat as noise to keep the tail readable.
      if (event.rate_rx_bps + event.rate_tx_bps < 1024) return null
      return {
        id: nextId++,
        tsMs: event.ts_ms,
        kind: event.type,
        tone: "muted",
        text: `server.sample · ${event.server_id} · ${event.online_count}/${event.peer_count} peers · ${formatBps(event.rate_tx_bps)} tx`,
      }
    case "heartbeat":
      return null
  }
  // Event variants not surfaced in the tail (e.g. data-change syncs).
  return null
}

/**
 * Rolling tail of real WebSocket events. The same upstream broadcast the
 * dashboard uses for live throughput populates this tail — every line is a
 * backend-emitted event, never a mock. Consumed by:
 *   - <LiveEventStream> (full terminal tail panel)
 *   - <RecentActivity>  (for non-admin users, where /admin/audit isn't
 *                       reachable — we derive a per-user activity feed from
 *                       their own filtered events)
 */
export const useEventTail = create<EventTailState>((set) => ({
  lines: [],
  connectedAt: null,
  push(event) {
    const line = toLine(event)
    if (!line) return
    set((state) => {
      const next = state.lines.length < TAIL_CAP
        ? [...state.lines, line]
        : [...state.lines.slice(state.lines.length - TAIL_CAP + 1), line]
      return {
        lines: next,
        connectedAt: state.connectedAt ?? line.tsMs,
      }
    })
  },
  markConnected() {
    set({ connectedAt: Date.now() })
  },
  reset() {
    set({ lines: [], connectedAt: null })
  },
}))
