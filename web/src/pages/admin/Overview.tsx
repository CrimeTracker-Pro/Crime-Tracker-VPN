import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconActivity,
  IconArrowDown,
  IconArrowUp,
  IconInfoCircle,
  IconServer,
  IconUsers,
} from "@tabler/icons-react"
import { AnimatePresence, motion } from "motion/react"
import { useMemo } from "react"
import { toast } from "sonner"

import { CandleChart } from "@/components/charts/CandleChart"
import { LiveIndicator } from "@/components/charts/LiveIndicator"
import { PageStagger, StaggerItem } from "@/components/motion"
import { Kpi, KpiStrip, PageHead, Panel, Pill } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { WithTooltip } from "@/components/ui/with-tooltip"
import {
  ApiError,
  type AdminServerRow,
  adminFleetBandwidth,
  adminGetMaintenance,
  adminHostAgentStatus,
  adminListServers,
  adminSetMaintenance,
  adminStats,
} from "@/lib/api"
import { useReducedMotion } from "@/lib/motion"
import { formatBps, formatBytes } from "@/lib/units"
import { useLiveStats } from "@/stores/liveStats"

export function AdminOverviewPage() {
  const qc = useQueryClient()

  // Deployment-wide aggregate counts power the KPI strip below.
  const statsQ = useQuery({
    queryKey: ["admin", "stats"],
    queryFn: adminStats,
    refetchInterval: 30_000,
  })
  const hostAgentQ = useQuery({
    queryKey: ["admin", "host-agent-status"],
    queryFn: adminHostAgentStatus,
    enabled: import.meta.env.VITE_OPTION2_HOST_AGENT === "true",
    refetchInterval: 5_000,
  })

  // Fleet-wide RX/TX over the last 30 days for the bandwidth KPI.
  const fleetBwQ = useQuery({
    queryKey: ["admin", "fleet-bandwidth"],
    queryFn: adminFleetBandwidth,
    refetchInterval: 5 * 60_000,
  })

  const maintQ = useQuery({
    queryKey: ["admin", "maintenance"],
    queryFn: adminGetMaintenance,
  })

  // Per-server live cards: list servers; live `ServerSample` events from the
  // WS drive the KPI strip (Peers, Online, RX/TX rate) and each card's
  // <CandleChart> owns its own bandwidth history. The list itself just needs
  // to refresh occasionally to pick up server adds/removes/renames.
  const serversQ = useQuery({
    queryKey: ["admin", "servers", "overview"],
    queryFn: adminListServers,
    refetchInterval: 30_000,
  })

  const setMaintM = useMutation({
    mutationFn: (on: boolean) => adminSetMaintenance(on),
    onSuccess: (_d, on) => {
      void qc.invalidateQueries({ queryKey: ["admin", "maintenance"] })
      toast.info(on ? "Maintenance mode ON" : "Maintenance mode OFF")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const stats = statsQ.data
  const total = stats?.total ?? 0
  const active = stats?.active ?? 0
  const suspended = stats?.suspended ?? 0
  const pending = stats?.pending_verification ?? 0
  const totalDevices = stats?.devices_total ?? 0
  // Durable, DB-computed online count (same 180s handshake rule as the user
  // dashboard). Used as the KPI base so it's correct on first paint and
  // survives refresh; live WS ticks below can only refine it upward.
  const onlineDb = stats?.online_now ?? 0
  const fleetRx = fleetBwQ.data?.rx_bytes ?? 0
  const fleetTx = fleetBwQ.data?.tx_bytes ?? 0
  const maintOn = !!maintQ.data?.maintenance_mode

  // Sum the worker-reported `onlineCount` across every server — devices
  // with a recent WireGuard handshake. Reflects real-time connection
  // pressure on the fleet, vs. the cumulative "Devices · fleet" KPI to
  // its left.
  const liveServers = useLiveStats((s) => s.servers)
  const onlineNow = useMemo(
    () =>
      Object.values(liveServers).reduce(
        (acc, srv) => acc + (srv?.onlineCount ?? 0),
        0
      ),
    [liveServers]
  )

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Admin · 01"
          title="Overview"
          sub="health · users · activity · trust surfaces"
          right={
            <div className="flex items-center gap-2">
              <Button
                variant={maintOn ? "destructive" : "outline"}
                onClick={() => setMaintM.mutate(!maintOn)}
                disabled={setMaintM.isPending || maintQ.isLoading}
              >
                {maintOn ? "● Maintenance ON" : "Toggle maintenance"}
              </Button>
              <WithTooltip
                side="bottom"
                label={
                  <div className="max-w-xs space-y-1.5 font-mono text-[11px] leading-snug">
                    <p>
                      <span className="font-semibold">Maintenance mode</span>{" "}
                      flips a site-wide kill-switch.
                    </p>
                    <p>
                      When ON, the API rejects non-admin writes with{" "}
                      <code className="rounded-sm bg-amber-400/20 px-1 py-px font-semibold text-amber-300">
                        503
                      </code>{" "}
                      and the UI shows a banner. Admin sessions stay functional
                      so you can still recover.
                    </p>
                    <p className="text-background/60">
                      Reads continue normally · WS streams continue · existing
                      WG tunnels are untouched.
                    </p>
                  </div>
                }
              >
                <button
                  type="button"
                  aria-label="About maintenance mode"
                  className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:text-foreground focus-visible:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                >
                  <IconInfoCircle className="size-4" />
                </button>
              </WithTooltip>
            </div>
          }
        />
      </StaggerItem>

      {import.meta.env.VITE_OPTION2_HOST_AGENT === "true" && (
        <StaggerItem>
          <div role="status" className="rounded-md border border-border px-4 py-3 text-sm">
            <span className="font-semibold">Host VPN state: </span>
            {hostAgentQ.isError ? "status unavailable" :
              !hostAgentQ.data?.length ? "waiting for first reconciliation" :
              hostAgentQ.data.map((item) =>
                `${item.status} · applied ${item.applied_revision} / desired ${item.desired_revision}${item.last_error ? ` · ${item.last_error}` : ""}`
              ).join("; ")}
          </div>
        </StaggerItem>
      )}

      <StaggerItem>
        <KpiStrip>
          <Kpi
            label="Users · total"
            value={statsQ.isLoading ? "—" : total}
            footL={`${active} active${pending > 0 ? ` · ${pending} pending` : ""}`}
            footR={suspended > 0 ? `${suspended} suspended` : undefined}
            deltaTone={suspended > 0 ? "dn" : undefined}
          />
          <Kpi
            label="Devices · fleet"
            value={statsQ.isLoading ? "—" : totalDevices}
            footL="WireGuard peers · non-revoked"
          />
          <Kpi
            label="Fleet bandwidth · total"
            value={fleetBwQ.isLoading ? "—" : formatBytes(fleetRx + fleetTx)}
            footL={`RX ${formatBytes(fleetRx)} · TX ${formatBytes(fleetTx)}`}
          />
          <Kpi
            label="Online · now"
            value={statsQ.isLoading ? "—" : Math.max(onlineDb, onlineNow)}
            unit={totalDevices > 0 ? `/ ${totalDevices}` : undefined}
            footL={
              maintOn
                ? "writes blocked · maintenance ON"
                : "live handshakes · fleet"
            }
            footR={maintOn ? "● maintenance" : undefined}
          />
        </KpiStrip>
      </StaggerItem>

      {(serversQ.data ?? []).length > 0 && (
        <StaggerItem>
          <Panel
            title="Server live"
            sub="Last 5 minutes · streamed over WS · seeded from server_samples on mount"
            right={<LiveIndicator />}
            flush
          >
            <div className="flex flex-col">
              {(serversQ.data ?? []).map((srv, i) => (
                <ServerLiveCard key={srv.id} server={srv} divider={i > 0} />
              ))}
            </div>
          </Panel>
        </StaggerItem>
      )}
    </PageStagger>
  )
}

function ServerLiveCard({
  server,
  divider,
}: {
  server: AdminServerRow
  divider: boolean
}) {
  const live = useLiveStats((s) => s.servers[server.id])
  const reduceMotion = useReducedMotion()

  return (
    <div
      className={`flex flex-col gap-4 p-5 ${divider ? "border-t border-border" : ""}`}
    >
      {/* Header — server identity + active/disabled tag on the right */}
      <div className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <span className="flex size-8 shrink-0 items-center justify-center border border-border bg-muted/40">
            <IconServer className="size-4 text-muted-foreground" />
          </span>
          <div className="flex min-w-0 flex-col">
            <span className="truncate font-mono text-sm font-medium text-foreground">
              {server.name}
            </span>
            <span className="truncate font-mono text-[11px] text-muted-foreground">
              {server.region} · {server.endpoint_host}:{server.endpoint_port}
            </span>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone={server.is_active ? "ok" : "warn"} dot={false}>
            {server.is_active ? "active" : "disabled"}
          </Pill>
        </div>
      </div>

      {/* KPI strip — Peers · Online · RX/TX live rate, streamed from
          `server_sample` over the WS. Each value pop-animates when it
          changes so updates are obvious. */}
      <div className="grid grid-cols-2 border border-border md:grid-cols-4">
        <LiveStat
          label="Peers"
          value={live?.peerCount ?? 0}
          icon={<IconUsers className="size-3.5" />}
          reduceMotion={reduceMotion}
        />
        <LiveStat
          label="Online"
          value={live?.onlineCount ?? 0}
          icon={<IconActivity className="size-3.5" />}
          tone={live?.onlineCount ? "ok" : "neutral"}
          reduceMotion={reduceMotion}
        />
        <LiveStat
          label="RX"
          value={formatBps(live?.rxBps ?? 0)}
          icon={<IconArrowDown className="size-3.5 text-[var(--chart-1)]" />}
          reduceMotion={reduceMotion}
        />
        <LiveStat
          label="TX"
          value={formatBps(live?.txBps ?? 0)}
          icon={<IconArrowUp className="size-3.5 text-primary" />}
          reduceMotion={reduceMotion}
        />
      </div>

      {/* Bandwidth candles — server-aggregate OHLC across all peers. The
          chart owns its timeframe + zoom/pan/lazy-history controls and is the
          single throughput-over-time view (the old live RX/TX line chart was
          redundant with this and has been removed). */}
      <div className="flex flex-col gap-2">
        <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
          Bandwidth · scroll to zoom · drag to pan
        </span>
        <CandleChart scope="server" id={server.id} height={260} />
      </div>
    </div>
  )
}

/**
 * Inline KPI cell with a pop-in transition on value change. Wrapping
 * the displayed value in `<AnimatePresence mode="popLayout">` swaps in
 * each new number with a brief fade+rise so live updates are visually
 * announced without being noisy. Honors prefers-reduced-motion.
 */
function LiveStat({
  label,
  value,
  icon,
  tone = "neutral",
  reduceMotion,
}: {
  label: string
  value: number | string
  icon?: React.ReactNode
  tone?: "ok" | "neutral"
  reduceMotion: boolean | null
}) {
  return (
    <div className="flex flex-col gap-1 border-r border-border p-3 last:border-r-0 md:last:border-r-0 [&:nth-child(2n)]:border-r-0 md:[&:nth-child(2n)]:border-r">
      <span className="inline-flex items-center gap-1.5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
        {icon}
        {label}
      </span>
      <div className="overflow-hidden">
        <AnimatePresence mode="popLayout" initial={false}>
          <motion.span
            key={String(value)}
            initial={reduceMotion ? { opacity: 1 } : { opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduceMotion ? { opacity: 0 } : { opacity: 0, y: -6 }}
            transition={{ duration: 0.18, ease: [0.16, 1, 0.3, 1] }}
            className={`block font-mono text-base tabular-nums ${tone === "ok" ? "text-status-online" : "text-foreground"}`}
          >
            {value}
          </motion.span>
        </AnimatePresence>
      </div>
    </div>
  )
}
