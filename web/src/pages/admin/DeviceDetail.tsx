import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconArrowLeft,
  IconPlayerPause,
  IconPlayerPlay,
  IconTrash,
} from "@tabler/icons-react"
import { useState } from "react"
import { Link, useParams } from "react-router"
import { toast } from "sonner"

import { CandleChart } from "@/components/charts/CandleChart"
import { ConfirmDialog } from "@/components/ConfirmDialog"
import { CopyableCode } from "@/components/CopyableCode"
import { PageStagger, StaggerItem } from "@/components/motion"
import { RelativeTime } from "@/components/RelativeTime"
import {
  Eyebrow,
  Kbd,
  Kpi,
  KpiStrip,
  PageHead,
  Panel,
  Pill,
  type PillTone,
} from "@/components/swiss"
import { StatusPill } from "@/components/StatusPill"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import {
  ApiError,
  type ConnectionSessionRow,
  type EndpointHistoryRow,
  type PublicDevice,
  adminGetDeviceDetail,
  adminListDeviceConnectionHistory,
  adminListDeviceEndpointHistory,
  adminPauseDevice,
  adminRevokeDevice,
  adminUnpauseDevice,
} from "@/lib/api"
import { DEVICE_TYPE_ICONS, deviceTypeLabel, osLabel } from "@/lib/deviceIcons"
import { formatBps, formatBytes } from "@/lib/units"
import { useDeviceOnline } from "@/hooks/useDeviceOnline"
import { useLiveTotal } from "@/hooks/useLiveTotal"
import { useLiveStats } from "@/stores/liveStats"

/**
 * Admin device detail. Mirrors the user-facing device page's layout — a live
 * RX / TX / Total / Quota KPI strip and the OHLC bandwidth candle chart —
 * plus the admin moderation controls (pause / resume / revoke) so an abusive
 * or compromised peer can be stopped without impersonating its owner. Admin
 * depth (owner, endpoint history, connection sessions, activity, identifiers)
 * lives below.
 */
export function AdminDeviceDetailPage() {
  const { id = "" } = useParams<{ id: string }>()
  const detailQ = useQuery({
    queryKey: ["admin", "device", id],
    queryFn: () => adminGetDeviceDetail(id),
    enabled: id.length > 0,
    // Keep status / totals / quota fresh without a manual refresh (the user
    // page polls the same way; live WS deltas tick the KPIs in between).
    refetchInterval: 20_000,
  })
  const endpointQ = useQuery({
    queryKey: ["admin", "device", id, "endpoint-history"],
    queryFn: () => adminListDeviceEndpointHistory(id),
    enabled: id.length > 0,
  })
  const connectionQ = useQuery({
    queryKey: ["admin", "device", id, "connection-history"],
    queryFn: () => adminListDeviceConnectionHistory(id),
    enabled: id.length > 0,
  })

  const d = detailQ.data?.device
  const owner = detailQ.data?.owner
  const endpoints: EndpointHistoryRow[] = endpointQ.data ?? []
  const connections: ConnectionSessionRow[] = connectionQ.data ?? []

  // Live wiring — identical to the user page. Admins receive `stats_delta` for
  // every device over the WS (server-side `visible_to`), so the live store has
  // this device's throughput + lifetime totals keyed by id. Hooks run
  // unconditionally (before the conditional render) to keep hook order stable.
  const live = useLiveStats((s) => s.devices[id])
  const { rx: totalRx, tx: totalTx } = useLiveTotal(
    id,
    d?.total_rx_bytes ?? 0,
    d?.total_tx_bytes ?? 0
  )
  const isOnline = useDeviceOnline((d as PublicDevice | undefined) ?? null)

  const TypeIcon = d ? DEVICE_TYPE_ICONS[d.device_type] : null
  const rxHistory = live?.rxHistory ?? []
  const txHistory = live?.txHistory ?? []
  const totalHistory = rxHistory.map((v, i) => v + (txHistory[i] ?? 0))
  const isPaused = d?.status === "paused"
  const isRevoked = d?.status === "revoked"

  // ── Moderation actions ────────────────────────────────────────────────
  const qc = useQueryClient()
  const [revokeOpen, setRevokeOpen] = useState(false)
  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: ["admin", "device", id] })
    void qc.invalidateQueries({ queryKey: ["admin", "devices"] })
  }
  const onActionError = (e: unknown) => {
    if (e instanceof ApiError) toast.error(e.message)
  }
  const pauseM = useMutation({
    mutationFn: () =>
      isPaused ? adminUnpauseDevice(id) : adminPauseDevice(id),
    onSuccess: () => {
      invalidate()
      toast.success(isPaused ? "Device resumed" : "Device paused")
    },
    onError: onActionError,
  })
  const revokeM = useMutation({
    mutationFn: () => adminRevokeDevice(id),
    onSuccess: () => {
      invalidate()
      setRevokeOpen(false)
      toast.success("Device revoked — IP released and peer removed")
    },
    onError: onActionError,
  })

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow={
            d
              ? `${deviceTypeLabel(d.device_type)} · ${osLabel(d.os)} · ${d.id
                  .slice(0, 8)
                  .toUpperCase()}`
              : "Admin · device"
          }
          title={
            d ? (
              <span className="inline-flex items-center gap-2">
                {TypeIcon && (
                  <TypeIcon className="size-[0.8em] shrink-0 text-muted-foreground" />
                )}
                <span className="min-w-0 break-words">{d.name}</span>
              </span>
            ) : detailQ.isLoading ? (
              "Loading…"
            ) : (
              "Unknown device"
            )
          }
          sub={
            d ? (
              <span className="inline-flex flex-wrap items-center gap-x-1.5 gap-y-1">
                <span className="font-mono">{d.allocated_ip}</span>
                {owner && (
                  <span className="text-muted-foreground/70">
                    · owner {owner.email}
                  </span>
                )}
              </span>
            ) : detailQ.isLoading ? undefined : (
              "Device not found"
            )
          }
          right={
            <div className="flex items-center gap-2">
              {owner && (
                <Button asChild size="sm" variant="outline">
                  <Link to={`/admin/users/${owner.id}`}>
                    <IconArrowLeft className="size-3.5" />
                    Back to {owner.email.split("@")[0]}
                  </Link>
                </Button>
              )}
              {d && !isRevoked && (
                <>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={pauseM.isPending}
                    onClick={() => pauseM.mutate()}
                  >
                    {isPaused ? (
                      <>
                        <IconPlayerPlay className="size-3.5" />
                        Resume
                      </>
                    ) : (
                      <>
                        <IconPlayerPause className="size-3.5" />
                        Pause
                      </>
                    )}
                  </Button>
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={() => setRevokeOpen(true)}
                  >
                    <IconTrash className="size-3.5" />
                    Revoke
                  </Button>
                </>
              )}
              {d && (
                <StatusPill
                  status={
                    isRevoked
                      ? "revoked"
                      : isPaused
                        ? "paused"
                        : isOnline
                          ? "online"
                          : "offline"
                  }
                />
              )}
            </div>
          }
        />
      </StaggerItem>

      {detailQ.isError && (
        <StaggerItem>
          <Panel title="Failed to load">
            <p className="text-sm text-muted-foreground">
              {detailQ.error instanceof ApiError
                ? detailQ.error.message
                : "Could not fetch device."}
            </p>
          </Panel>
        </StaggerItem>
      )}

      {detailQ.isLoading && (
        <StaggerItem>
          <Skeleton className="h-32 rounded-none" />
        </StaggerItem>
      )}

      {d && owner && (
        <>
          {/* KPI strip — mirrors the user device page: RX live, TX live,
              Total (lifetime), Quota. */}
          <StaggerItem>
            <KpiStrip>
              <Kpi
                label="RX"
                value={
                  <span className="tabular-nums">{formatBytes(totalRx)}</span>
                }
                spark={isOnline ? rxHistory.slice(-32) : []}
                sparkColor="var(--chart-1)"
                footL={isOnline ? formatBps(live?.rxBps ?? 0) : "idle"}
                footR={isOnline ? "live" : undefined}
                deltaTone={isOnline ? "up" : undefined}
              />
              <Kpi
                label="TX"
                value={
                  <span className="tabular-nums">{formatBytes(totalTx)}</span>
                }
                spark={isOnline ? txHistory.slice(-32) : []}
                sparkColor="var(--primary)"
                footL={isOnline ? formatBps(live?.txBps ?? 0) : "idle"}
                footR={isOnline ? "live" : undefined}
                deltaTone={isOnline ? "up" : undefined}
              />
              <Kpi
                label="Total"
                value={
                  <span className="tabular-nums">
                    {formatBytes(totalRx + totalTx)}
                  </span>
                }
                spark={isOnline ? totalHistory.slice(-32) : []}
                sparkColor="var(--primary)"
                footL="rx + tx"
              />
              <QuotaKpi
                used={d.current_month_bytes}
                cap={d.monthly_byte_cap}
                autoPaused={d.auto_paused}
              />
            </KpiStrip>
          </StaggerItem>

          {/* Bandwidth — same self-contained OHLC candle chart as the user
              page (admin-scoped data source). */}
          <StaggerItem>
            <CandleChart
              scope="admin-device"
              id={id}
              height={260}
              title="Bandwidth"
              sub="scroll to zoom · drag to pan"
            />
          </StaggerItem>

          {/* ── Admin depth below ─────────────────────────────────────── */}

          <StaggerItem>
            <Panel
              title="Configuration"
              sub="Public key, peer overrides (read-only)"
            >
              <div className="grid gap-4 sm:grid-cols-2">
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Public key</Eyebrow>
                  <CopyableCode value={d.public_key} />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Allocated IP</Eyebrow>
                  <CopyableCode value={d.allocated_ip} />
                </div>
              </div>
            </Panel>
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Endpoint history"
              sub={`${endpoints.length}${endpoints.length === 200 ? "+" : ""} distinct WG endpoints, newest first`}
              flush
            >
              {endpointQ.isLoading ? (
                <Skeleton className="h-20 rounded-none" />
              ) : endpoints.length > 0 ? (
                <div className="zv-table-scroll">
                  <table className="zv-table">
                    <thead>
                      <tr>
                        <th>Endpoint</th>
                        <th className="w-[200px]">First seen</th>
                      </tr>
                    </thead>
                    <tbody>
                      {endpoints.slice(0, 50).map((r) => (
                        <tr key={r.id}>
                          <td className="font-mono text-xs">{r.endpoint}</td>
                          <td className="font-mono text-xs text-muted-foreground">
                            <RelativeTime value={r.observed_at} />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                  No endpoints captured yet.
                </div>
              )}
              {endpoints.length > 50 && (
                <p className="p-2 font-mono text-[11px] text-muted-foreground">
                  Showing first 50 of {endpoints.length}.
                </p>
              )}
            </Panel>
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Connection sessions"
              sub={`${connections.length}${connections.length === 200 ? "+" : ""} sessions, newest first · open sessions show "active"`}
              flush
            >
              {connectionQ.isLoading ? (
                <Skeleton className="h-20 rounded-none" />
              ) : connections.length > 0 ? (
                <div className="zv-table-scroll">
                  <table className="zv-table">
                    <thead>
                      <tr>
                        <th className="w-[180px]">Started</th>
                        <th className="w-[110px]">Duration</th>
                        <th>Endpoint</th>
                        <th className="text-right">RX</th>
                        <th className="text-right">TX</th>
                      </tr>
                    </thead>
                    <tbody>
                      {connections.slice(0, 50).map((s) => {
                        const open = s.ended_at == null
                        const duration = open
                          ? "active"
                          : formatDuration(s.started_at, s.ended_at!)
                        const rx =
                          s.rx_bytes_at_end != null
                            ? Math.max(
                                0,
                                s.rx_bytes_at_end - s.rx_bytes_at_start
                              )
                            : null
                        const tx =
                          s.tx_bytes_at_end != null
                            ? Math.max(
                                0,
                                s.tx_bytes_at_end - s.tx_bytes_at_start
                              )
                            : null
                        const endpointChanged =
                          !open &&
                          s.peer_endpoint_at_end != null &&
                          s.peer_endpoint_at_end !== s.peer_endpoint_at_start
                        return (
                          <tr key={s.id}>
                            <td className="font-mono text-xs text-muted-foreground">
                              <RelativeTime value={s.started_at} />
                            </td>
                            <td className="font-mono text-xs tabular-nums">
                              {open ? (
                                <Pill tone="ok" dot>
                                  active
                                </Pill>
                              ) : (
                                duration
                              )}
                            </td>
                            <td className="font-mono text-xs">
                              {s.peer_endpoint_at_start ?? (
                                <span className="text-muted-foreground">—</span>
                              )}
                              {endpointChanged && (
                                <span
                                  className="text-muted-foreground"
                                  title={`Ended on ${s.peer_endpoint_at_end}`}
                                >
                                  {" → "}
                                  {s.peer_endpoint_at_end}
                                </span>
                              )}
                            </td>
                            <td className="text-right font-mono text-xs text-muted-foreground tabular-nums">
                              {rx != null ? (
                                formatBytes(rx)
                              ) : (
                                <span className="text-muted-foreground">—</span>
                              )}
                            </td>
                            <td className="text-right font-mono text-xs text-muted-foreground tabular-nums">
                              {tx != null ? (
                                formatBytes(tx)
                              ) : (
                                <span className="text-muted-foreground">—</span>
                              )}
                            </td>
                          </tr>
                        )
                      })}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                  No connection sessions yet.
                </div>
              )}
              {connections.length > 50 && (
                <p className="p-2 font-mono text-[11px] text-muted-foreground">
                  Showing first 50 of {connections.length}.
                </p>
              )}
            </Panel>
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Recent activity"
              sub="Audit entries targeting this device, newest first"
              flush
            >
              {detailQ.data && detailQ.data.activity.length > 0 ? (
                <div className="zv-table-scroll">
                  <table className="zv-table">
                    <thead>
                      <tr>
                        <th>Action</th>
                        <th>Metadata</th>
                        <th>When</th>
                      </tr>
                    </thead>
                    <tbody>
                      {detailQ.data.activity.map((a) => (
                        <tr key={a.id}>
                          <td>
                            <Pill tone={toneForAction(a.action)} dot={false}>
                              {a.action}
                            </Pill>
                          </td>
                          <td className="font-mono text-[11px] text-muted-foreground">
                            <Kbd>{summarize(a.metadata)}</Kbd>
                          </td>
                          <td className="font-mono text-xs text-muted-foreground">
                            <RelativeTime value={a.created_at} fallback="—" />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                  No activity.
                </div>
              )}
            </Panel>
          </StaggerItem>

          <StaggerItem>
            <Panel title="Identifiers" sub="For ops / debugging">
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Device id</Eyebrow>
                  <CopyableCode value={d.id} />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Server id</Eyebrow>
                  <CopyableCode value={d.server_id} />
                </div>
              </div>
            </Panel>
          </StaggerItem>
        </>
      )}
      <ConfirmDialog
        open={revokeOpen}
        onOpenChange={setRevokeOpen}
        title="Revoke this device?"
        description={
          d
            ? `Permanently revokes ${d.name} (owner ${owner?.email ?? "unknown"}): the WG peer is removed and its IP is released for reallocation. The owner keeps their account and other devices.`
            : undefined
        }
        confirmLabel="Revoke device"
        destructive
        confirmText={d?.name}
        pending={revokeM.isPending}
        onConfirm={() => revokeM.mutate()}
      />
    </PageStagger>
  )
}

/** "Quota" KPI card — this device's month-to-date usage against its per-device
 *  cap (green < 70%, amber ≥ 70%, red ≥ 90%); shows usage + "no cap" when
 *  uncapped, and flags an auto-pause. Mirrors the user device page. */
function QuotaKpi({
  used,
  cap,
  autoPaused,
}: {
  used: number
  cap: number | null
  autoPaused: boolean
}) {
  const cap0 = cap ?? 0
  const hasCap = cap0 > 0
  const pct = hasCap ? Math.min(100, Math.round((used / cap0) * 100)) : 0
  const tone =
    pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-500" : "bg-emerald-500"
  return (
    <div className="zv-kpi">
      <div className="zv-kpi-label">
        <span>Quota</span>
      </div>
      <div className="zv-kpi-val font-heading">
        <span className="tabular-nums">{formatBytes(used)}</span>
      </div>
      <div className="mt-1.5 h-1.5 w-full overflow-hidden bg-muted">
        {hasCap && (
          <div className={`h-full ${tone}`} style={{ width: `${pct}%` }} />
        )}
      </div>
      <div className="zv-kpi-foot">
        <span>
          {hasCap ? `/ ${formatBytes(cap0)} this month` : "this month"}
        </span>
        <span
          className={
            autoPaused ? "text-amber-600 dark:text-amber-400" : undefined
          }
        >
          {autoPaused ? "auto-paused" : hasCap ? `${pct}%` : "no cap"}
        </span>
      </div>
    </div>
  )
}

function formatDuration(startIso: string, endIso: string): string {
  const ms = new Date(endIso).getTime() - new Date(startIso).getTime()
  if (!Number.isFinite(ms) || ms < 0) return "—"
  const totalSec = Math.floor(ms / 1000)
  if (totalSec < 60) return `${totalSec}s`
  const totalMin = Math.floor(totalSec / 60)
  if (totalMin < 60) {
    const s = totalSec % 60
    return `${totalMin}m ${s.toString().padStart(2, "0")}s`
  }
  const h = Math.floor(totalMin / 60)
  const m = totalMin % 60
  return `${h}h ${m.toString().padStart(2, "0")}m`
}

function summarize(metadata: unknown): string {
  if (metadata == null || typeof metadata !== "object") return "—"
  const obj = metadata as Record<string, unknown>
  if (Object.keys(obj).length === 0) return "—"
  const parts: string[] = []
  for (const [k, v] of Object.entries(obj)) {
    if (v == null) continue
    if (typeof v === "object") continue
    parts.push(`${k}=${String(v)}`)
    if (parts.length >= 3) break
  }
  return parts.length > 0 ? parts.join(" · ") : "—"
}

function toneForAction(action: string): PillTone {
  if (action.endsWith(".online")) return "ok"
  if (action.endsWith(".offline")) return "neutral"
  if (action.includes("revoke") || action.includes("delete")) return "err"
  if (action.includes("pause") || action.includes("disable")) return "warn"
  if (action.includes("create")) return "ok"
  return "info"
}
