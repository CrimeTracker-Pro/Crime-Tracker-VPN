import { useEffect, useRef } from "react"

import { useEventTail } from "@/stores/eventTail"

function pad2(n: number) {
  return String(n).padStart(2, "0")
}

function formatTs(tsMs: number, baseMs: number) {
  const diff = Math.max(0, Math.floor((tsMs - baseMs) / 1000))
  return `+${pad2(Math.floor(diff / 60))}:${pad2(diff % 60)}`
}

const TONE_DOT: Record<string, string> = {
  ok: "var(--status-online)",
  warn: "var(--status-degraded)",
  err: "var(--destructive)",
  info: "#5a9cff",
  muted: "var(--muted-foreground)",
}

/**
 * Terminal-style event tail. Reads from the shared event-tail store
 * populated by the main WebSocket connection. Every line is a real
 * backend-emitted event (stats_delta / handshake_change / peer_status_changed
 * / server_health for admins).
 */
export function LiveEventStream() {
  const lines = useEventTail((s) => s.lines)
  const connectedAt = useEventTail((s) => s.connectedAt)
  const scrollRef = useRef<HTMLDivElement>(null)

  // Anchor relative timestamps to the first line we ever saw. The cursor row
  // shows the latest line's time (not wall-clock "now" — render must stay
  // pure); with no lines yet both collapse to +0s.
  const lastMs = lines[lines.length - 1]?.tsMs
  const baseMs = connectedAt ?? lines[0]?.tsMs ?? 0

  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    el.scrollTop = el.scrollHeight
  }, [lines.length])

  return (
    <div
      ref={scrollRef}
      // Fixed height keeps the panel balanced with `<RecentActivity>` to
      // its left in the dashboard grid — the scroll container ALWAYS has
      // the same footprint whether there are 0 lines or 60.
      className="h-[420px] overflow-y-auto py-2 font-mono text-[11px] leading-[1.55] text-muted-foreground"
    >
      {lines.length === 0 && (
        <div className="px-4 py-2 text-muted-foreground/70">
          waiting for first event…
        </div>
      )}
      {lines.map((ln) => (
        <div key={ln.id} className="flex items-baseline gap-2 px-4 py-[2px]">
          <span className="w-12 shrink-0 text-muted-foreground/60">
            {formatTs(ln.tsMs, baseMs)}
          </span>
          <span
            className="shrink-0"
            style={{ color: TONE_DOT[ln.tone] }}
            aria-hidden
          >
            ●
          </span>
          <span className="truncate text-foreground/85">{ln.text}</span>
        </div>
      ))}
      <div className="flex items-baseline gap-2 px-4 py-[2px]">
        <span className="w-12 shrink-0 text-muted-foreground/60">
          {formatTs(lastMs ?? baseMs, baseMs)}
        </span>
        <span className="shrink-0 text-primary" aria-hidden>
          ▌
        </span>
      </div>
    </div>
  )
}
