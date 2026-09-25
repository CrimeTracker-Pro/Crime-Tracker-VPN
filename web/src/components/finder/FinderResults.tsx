import {
  IconArrowDown,
  IconArrowUp,
  IconChevronDown,
  IconChevronRight,
  IconDeviceDesktop,
  IconNetwork,
  IconUser,
} from "@tabler/icons-react"
import { useMemo, useState } from "react"
import { Link } from "react-router"

import { ChartPlaceholder } from "@/components/DeviceCard"
import { MiniAreaChart } from "@/components/charts/LazyMiniAreaChart"
import { useNow } from "@/hooks/useNow"
import { formatBps } from "@/lib/units"
import { useLiveStats } from "@/stores/liveStats"

/* ── Shared building blocks for Finder results ─────────────────────────
   Finder resolves IPs to devices and presents them the same way:
   an accordion section per owning user, with a compact live card per
   device (name · IP · RX/TX rate · throughput sparkline). */

/** Frames fed to the sparkline — matches DeviceCard's window. */
const CHART_WINDOW = 30
/** Live-activity heuristic for fleet devices where we don't have a full
 *  PublicDevice (admin finder matches): traffic within the keepalive
 *  window (~3 misses at 30 s) means the peer is up. */
const LIVE_STALE_MS = 90_000

/**
 * Compact live device card: name, VPN IP, current RX/TX rate, and a
 * throughput sparkline that streams from the live-stats store.
 *
 * `online` — pass an explicit state when available. Omit it to use the
 * live-frame heuristic; pass null when this caller has no visibility into
 * another device's live status.
 */
export function FinderDeviceCard({
  deviceId,
  name,
  ip,
  to,
  online,
  note,
}: {
  deviceId: string
  name: string
  ip: string
  /** Optional detail-page link; other users cannot open admin device pages. */
  to?: string
  /** null means the caller cannot see this device's live status. */
  online?: boolean | null
  /** Small trailing annotation, e.g. `matched on last_peer_endpoint`. */
  note?: string
}) {
  const live = useLiveStats((s) => s.devices[deviceId])
  const now = useNow()
  const statusUnavailable = online === null
  const isOnline = !statusUnavailable && (
    online ?? (live != null && now - live.lastSeenTs < LIVE_STALE_MS && live.lastSeenTs > 0)
  )

  const rxBps = isOnline ? (live?.rxBps ?? 0) : 0
  const txBps = isOnline ? (live?.txBps ?? 0) : 0
  const rxHistory = useMemo(
    () => (isOnline ? (live?.rxHistory ?? []).slice(-CHART_WINDOW) : []),
    [isOnline, live?.rxHistory],
  )
  const txHistory = useMemo(
    () => (isOnline ? (live?.txHistory ?? []).slice(-CHART_WINDOW) : []),
    [isOnline, live?.txHistory],
  )
  const showChart = isOnline && (rxHistory.length > 0 || txHistory.length > 0)

  const content = (
    <>
      <div className="flex items-center gap-3 px-3 pt-2.5">
        <IconDeviceDesktop className="text-muted-foreground size-4 shrink-0" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium">
          {name}
        </span>
        <span
          className="inline-flex shrink-0 items-center gap-1.5 font-mono text-[10px] uppercase tracking-wide"
          style={{
            color: isOnline ? "var(--status-online)" : "var(--muted-foreground)",
          }}
        >
          <span
            className="size-1.5 rounded-full"
            style={{
              background: isOnline
                ? "var(--status-online)"
                : "var(--status-offline)",
            }}
          />
          {statusUnavailable ? "status unavailable" : isOnline ? "online" : "offline"}
        </span>
      </div>
      <div className="text-muted-foreground flex items-center justify-between gap-3 px-3 pb-1.5 pt-1 font-mono text-[11px] tabular-nums">
        <span className="inline-flex min-w-0 items-center gap-1.5">
          <IconNetwork className="size-3 shrink-0" />
          <span className="truncate">{ip}</span>
          {note && <span className="truncate opacity-60">· {note}</span>}
        </span>
        <span className="inline-flex shrink-0 items-center gap-2">
          <span className="inline-flex items-center gap-0.5">
            <IconArrowDown className="size-2.5" />
            {isOnline ? formatBps(rxBps) : "—"}
          </span>
          <span className="inline-flex items-center gap-0.5">
            <IconArrowUp className="size-2.5" />
            {isOnline ? formatBps(txBps) : "—"}
          </span>
        </span>
      </div>
      {/* Fixed-height clipped strip, same reasoning as DeviceCard — keeps
          ResponsiveContainer from nudging the card height per frame. */}
      <div className="h-10 overflow-hidden px-1">
        {showChart ? (
          <MiniAreaChart rxHistory={rxHistory} txHistory={txHistory} height={40} />
        ) : (
          <ChartPlaceholder
            text={statusUnavailable ? "live traffic unavailable" : isOnline ? "connecting…" : "no live traffic"}
            height={40}
          />
        )}
      </div>
    </>
  )
  const className = "border-border bg-card group block border"
  return to ? (
    <Link
      to={to}
      className={`${className} hover:border-foreground/30 transition-colors`}
    >
      {content}
    </Link>
  ) : (
    <div className={className}>{content}</div>
  )
}

/**
 * Accordion section for one owning user. Header shows the email plus a
 * device count; the body renders the caller-supplied device cards in a
 * responsive grid. Open by default — collapsing is for scanning long
 * multi-user result sets.
 */
export function OwnerAccordion({
  email,
  count,
  to,
  defaultOpen = true,
  children,
}: {
  email: string
  count: number
  /** Optional owner deep-link (admin: /admin/users/:id). */
  to?: string
  defaultOpen?: boolean
  children: React.ReactNode
}) {
  const [open, setOpen] = useState(defaultOpen)
  return (
    <div className="border-border border">
      <div className="bg-muted/30 flex items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          className="text-muted-foreground hover:text-foreground flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          {open ? (
            <IconChevronDown className="size-3.5 shrink-0" />
          ) : (
            <IconChevronRight className="size-3.5 shrink-0" />
          )}
          <IconUser className="size-3.5 shrink-0" />
          <span className="text-foreground truncate font-mono text-sm">
            {email}
          </span>
          <span className="shrink-0 font-mono text-[10px] uppercase tracking-[0.08em]">
            {count} device{count === 1 ? "" : "s"}
          </span>
        </button>
        {to && (
          <Link
            to={to}
            className="text-muted-foreground hover:text-foreground shrink-0"
            title="Open user"
          >
            <IconChevronRight className="size-4" />
          </Link>
        )}
      </div>
      {open && (
        <div className="grid grid-cols-1 gap-2 p-2 sm:grid-cols-2 lg:grid-cols-3">
          {children}
        </div>
      )}
    </div>
  )
}
