import { useQuery } from "@tanstack/react-query"
import { IconDeviceTablet, IconPlus } from "@tabler/icons-react"
import { useMemo, useState } from "react"
import { Link, useNavigate } from "react-router"

import { LiveIndicator } from "@/components/charts/LiveIndicator"
import { CandleChart } from "@/components/charts/CandleChart"
import { LiveEventStream } from "@/components/dashboard/LiveEventStream"
import { RecentActivity } from "@/components/dashboard/RecentActivity"
import { AddDeviceDialog } from "@/components/devices/AddDeviceDialog"
import { EmptyState } from "@/components/EmptyState"
import { PageStagger, StaggerItem } from "@/components/motion"
import { Kpi, KpiStrip, PageHead, Panel } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Sheet, SheetTrigger } from "@/components/ui/sheet"
import { listDevices, myUsage } from "@/lib/api"
import { formatDate } from "@/lib/datetime"
import { connState } from "@/lib/deviceState"
import { formatBytes } from "@/lib/units"
import { useDeviceDetailGated } from "@/hooks/useDeviceDetailGated"
import { useHistoryHydration } from "@/hooks/useHistoryHydration"
import { useAuth } from "@/stores/auth"
import { useLiveStats } from "@/stores/liveStats"

export function DashboardPage() {
  const navigate = useNavigate()
  const user = useAuth((s) => s.user)
  const isAdmin = user?.role === "admin"
  const hideDetail = useDeviceDetailGated()

  const devicesQ = useQuery({ queryKey: ["devices"], queryFn: listDevices })
  // Hydrate the rolling per-device live history from the server-side
  // tick-level samples so the chart isn't empty after a refresh. The hook
  // fires WS-aware merges so any live deltas that landed before the
  // fetch completed aren't lost.
  const deviceIds = useMemo(
    () =>
      (devicesQ.data ?? [])
        .filter((d) => d.status === "active" || d.status === "paused")
        .map((d) => d.id),
    [devicesQ.data]
  )
  useHistoryHydration({ deviceIds, windowSec: 300 })
  const [addOpen, setAddOpen] = useState(false)

  const liveDevices = useLiveStats((s) => s.devices)
  const rates = useMemo(() => {
    const m = new Map<string, { rxBps: number; txBps: number }>()
    for (const [id, d] of Object.entries(liveDevices)) {
      m.set(id, { rxBps: d.rxBps, txBps: d.txBps })
    }
    return m
  }, [liveDevices])

  // Account-level monthly quota for the "Quota" KPI card.
  const usageQ = useQuery({
    queryKey: ["me", "usage"],
    queryFn: myUsage,
    refetchInterval: 60_000,
  })

  // Stable reference so the totals/online memos below don't recompute on
  // every render just because `?? []` minted a fresh array.
  const devices = useMemo(() => devicesQ.data ?? [], [devicesQ.data])

  // Headline totals = the sum of every device's lifetime RX/TX — the exact
  // figure each device card shows — grown live by the WS store so the numbers
  // tick up in real time. Summing the persisted per-device lifetime totals
  // (not a 30-day aggregate window) keeps the dashboard consistent with the
  // device cards + detail pages. See useLiveTotal / device_totals.
  const totalRxBytes = useMemo(
    () =>
      devices.reduce(
        (s, d) =>
          s + Math.max(d.total_rx_bytes, liveDevices[d.id]?.lifeRxBytes ?? 0),
        0
      ),
    [devices, liveDevices]
  )
  const totalTxBytes = useMemo(
    () =>
      devices.reduce(
        (s, d) =>
          s + Math.max(d.total_tx_bytes, liveDevices[d.id]?.lifeTxBytes ?? 0),
        0
      ),
    [devices, liveDevices]
  )
  const totalUsageBytes = totalRxBytes + totalTxBytes

  // Account monthly quota for the "Quota" card. `cap == null` ⇒ unlimited.
  const quotaCap = usageQ.data?.monthly_byte_cap ?? null
  const quotaUsed = usageQ.data?.current_month_bytes ?? 0
  const quotaPct =
    quotaCap && quotaCap > 0
      ? Math.min(100, Math.round((quotaUsed / quotaCap) * 100))
      : null
  const quotaResetsAt = usageQ.data?.quota_resets_at

  // Only currently-online devices contribute to "live" numbers. A device
  // that's offline / paused / revoked cannot be transmitting RIGHT NOW,
  // so any rate the WS store still holds for it is stale and dropped.
  const onlineDeviceIds = useMemo(
    () => devices.filter((d) => connState(d) === "online").map((d) => d.id),
    [devices]
  )
  const onlineCount = onlineDeviceIds.length
  const totalRx = useMemo(() => {
    let s = 0
    for (const id of onlineDeviceIds) s += rates.get(id)?.rxBps ?? 0
    return s
  }, [onlineDeviceIds, rates])
  const totalTx = useMemo(() => {
    let s = 0
    for (const id of onlineDeviceIds) s += rates.get(id)?.txBps ?? 0
    return s
  }, [onlineDeviceIds, rates])

  // Sparkline history for the KPI + the side panel monitor: sum per-device
  // histories only for currently-online devices, right-aligned so a
  // device that just connected doesn't backfill zeros into the past.
  const liveHistory = useMemo(() => {
    const ids = onlineDeviceIds
    if (ids.length === 0)
      return { rx: [] as number[], tx: [] as number[], total: [] as number[] }
    let maxLen = 0
    const rxSlices: number[][] = []
    const txSlices: number[][] = []
    const window = 32
    for (const id of ids) {
      const d = liveDevices[id]
      if (!d) continue
      const rx = d.rxHistory.slice(-window)
      const tx = d.txHistory.slice(-window)
      if (rx.length === 0 && tx.length === 0) continue
      rxSlices.push(rx)
      txSlices.push(tx)
      maxLen = Math.max(maxLen, rx.length, tx.length)
    }
    if (maxLen === 0) return { rx: [], tx: [], total: [] }
    const rx = new Array<number>(maxLen).fill(0)
    const tx = new Array<number>(maxLen).fill(0)
    for (const s of rxSlices) {
      const off = maxLen - s.length
      for (let i = 0; i < s.length; i++) rx[off + i] += s[i]
    }
    for (const s of txSlices) {
      const off = maxLen - s.length
      for (let i = 0; i < s.length; i++) tx[off + i] += s[i]
    }
    // Combined per-second total (rx+tx) — the "Total usage" card's line.
    const total = rx.map((v, i) => v + tx[i])
    return { rx, tx, total }
  }, [onlineDeviceIds, liveDevices])

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead eyebrow="Workspace · 01" title="Dashboard" />
      </StaggerItem>

      {/* KPI strip — 4-up. Devices online · Usage (range) · TX · RX.
          Headline numbers are window-scoped totals from the user's
          bandwidth rollups so the dashboard reflects the same activity
          the chart below renders; the live bps rate sits in the footer
          so "right now" is still readable at a glance. */}
      <StaggerItem>
        <KpiStrip cols={5}>
          <Kpi
            label="Devices · online"
            value={onlineCount}
            unit={devices.length > 0 ? `/ ${devices.length}` : undefined}
            deltaTone={onlineCount > 0 ? "up" : undefined}
          />
          <Kpi
            label="Total RX"
            value={devicesQ.isLoading ? "—" : formatBytes(totalRxBytes)}
            spark={liveHistory.rx}
            sparkColor="var(--chart-1)"
            footL={
              onlineCount === 0
                ? "no online devices"
                : `live · ${formatRate(totalRx)}`
            }
          />
          <Kpi
            label="Total TX"
            value={devicesQ.isLoading ? "—" : formatBytes(totalTxBytes)}
            spark={liveHistory.tx}
            sparkColor="var(--primary)"
            footL={
              onlineCount === 0
                ? "no online devices"
                : `live · ${formatRate(totalTx)}`
            }
          />
          <Kpi
            label="Total usage"
            value={devicesQ.isLoading ? "—" : formatBytes(totalUsageBytes)}
            spark={liveHistory.total}
            sparkColor="var(--primary)"
          />
          <Kpi
            label="Quota"
            value={
              usageQ.isLoading
                ? "—"
                : quotaPct == null
                  ? "Unlimited"
                  : `${quotaPct}%`
            }
            // Usage-so-far and the reset date only mean something against a
            // cap — an unlimited account shows just "Unlimited" (the actual
            // consumption already lives in the Total usage card).
            footL={
              quotaPct == null
                ? undefined
                : `${formatBytes(quotaUsed)} / ${formatBytes(quotaCap ?? 0)}`
            }
            footR={
              quotaPct != null && quotaResetsAt
                ? `resets ${formatDate(quotaResetsAt)}`
                : undefined
            }
          />
        </KpiStrip>
      </StaggerItem>

      {/* Row 2: bandwidth — OHLC candle chart aggregated across all the user's
          devices. Owns its own timeframe + zoom/pan controls (same chart as
          the device detail + admin overview pages). */}
      <StaggerItem>
        <CandleChart
          scope="user"
          height={260}
          title="Bandwidth"
          sub="All devices · scroll to zoom · drag to pan"
        />
      </StaggerItem>

      {/* First-device empty state — surfaced only when there are zero
          devices, since the devices table is no longer on this page. The
          CTA opens the same side-sheet wizard used on /app/devices so the
          two surfaces stay in lockstep (OS tiles + step-2 QR). */}
      {devicesQ.data && devicesQ.data.length === 0 && (
        <StaggerItem>
          <Panel>
            <Sheet open={addOpen} onOpenChange={setAddOpen}>
              <EmptyState
                icon={IconDeviceTablet}
                title="No devices yet"
                description="Add your first device to receive a WireGuard config."
                action={
                  <SheetTrigger asChild>
                    <Button>
                      <IconPlus />
                      Add device
                    </Button>
                  </SheetTrigger>
                }
              />
              <AddDeviceDialog
                onCreated={(d) => {
                  setAddOpen(false)
                  if (!hideDetail) navigate(`/app/devices/${d.device.id}`)
                }}
              />
            </Sheet>
          </Panel>
        </StaggerItem>
      )}

      {/* Row 4: recent activity (left) + live event stream (right) */}
      <StaggerItem className="grid gap-6 lg:grid-cols-2">
        <Panel
          title="Recent activity"
          sub={isAdmin ? "audit · last 8" : "live events · last 8"}
          flush
          right={
            isAdmin ? (
              <Link
                to="/admin/audit"
                className="font-mono text-xs text-muted-foreground hover:text-foreground"
              >
                View all ↗
              </Link>
            ) : null
          }
        >
          <RecentActivity limit={8} />
        </Panel>

        <Panel
          title="Live event stream"
          sub="ws · /api/v1/ws"
          right={<LiveIndicator />}
          flush
        >
          <LiveEventStream />
        </Panel>
      </StaggerItem>
    </PageStagger>
  )
}

function formatRate(bps: number): string {
  if (bps < 1_000) return `${Math.round(bps)} bps`
  if (bps < 1_000_000) return `${(bps / 1_000).toFixed(1)} kbps`
  if (bps < 1_000_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`
  return `${(bps / 1_000_000_000).toFixed(2)} Gbps`
}
