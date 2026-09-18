import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import {
  IconBrandAndroid,
  IconBrandApple,
  IconBrandWindows,
  IconCheck,
  IconChevronDown,
  IconChevronUp,
  IconCopy,
  IconDeviceMobile,
  IconDownload,
  IconFingerprint,
  IconKey,
  IconListDetails,
  IconPencil,
  IconPlayerPause,
  IconPlayerPlay,
  IconPlugConnected,
  IconPlugConnectedX,
  IconRefresh,
  IconSettings,
  IconSparkles,
  IconTerminal2,
  IconTrash,
} from "@tabler/icons-react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useNavigate, useParams } from "react-router"
import { toast } from "sonner"

import { CandleChart } from "@/components/charts/CandleChart"
import { ConfirmDialog } from "@/components/ConfirmDialog"
import { CopyableCode } from "@/components/CopyableCode"
import { EditDeviceDialog } from "@/components/devices/EditDeviceDialog"
import { PageStagger, StaggerItem } from "@/components/motion"
import { RelativeTime } from "@/components/RelativeTime"
import {
  Eyebrow,
  Kpi,
  KpiStrip,
  PageHead,
  Panel,
  fmtRel,
} from "@/components/swiss"
import { StatusPill } from "@/components/StatusPill"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { WithTooltip } from "@/components/ui/with-tooltip"
import { useBreadcrumbOverride } from "@/hooks/useBreadcrumbOverride"
import { useDeviceOnline } from "@/hooks/useDeviceOnline"
import { useHistoryHydration } from "@/hooks/useHistoryHydration"
import { useLiveTotal } from "@/hooks/useLiveTotal"
import { useNow } from "@/hooks/useNow"
import {
  ApiError,
  type CreatedDevice,
  type DeviceEvent,
  type DeviceOs,
  deleteDevice,
  getDevice,
  listDeviceEvents,
  meServer,
  myUsage,
  pauseDevice,
  redownloadDeviceConf,
  rotateDeviceKeys,
  unpauseDevice,
} from "@/lib/api"
import { copyText } from "@/lib/clipboard"
import { DEVICE_TYPE_ICONS, deviceTypeLabel, osLabel } from "@/lib/deviceIcons"
import { formatDate } from "@/lib/datetime"
import { formatBps } from "@/lib/units"
import { cn } from "@/lib/utils"
import { useLiveStats } from "@/stores/liveStats"

const KEEPALIVE_SECS = 30

// Activity feed: how many recent entries the inline panel shows before the
// "Full view" sheet takes over, and the page size the sheet pages through.
const ACTIVITY_PREVIEW_COUNT = 10
const ACTIVITY_PAGE_SIZE = 30


export function DeviceDetailPage() {
  const { id = "" } = useParams()
  const navigate = useNavigate()
  const qc = useQueryClient()

  const deviceQ = useQuery({
    queryKey: ["device", id],
    queryFn: () => getDevice(id),
    enabled: id.length > 0,
    // The online/offline pill is derived from `last_handshake_at`, which the
    // worker advances as the peer re-handshakes. A WS `handshake_change`
    // event invalidates this query for an instant flip; this poll is the
    // fallback so the status still self-corrects if the socket drops.
    refetchInterval: 20_000,
  })
  const serverQ = useQuery({
    queryKey: ["me", "server"],
    queryFn: meServer,
    staleTime: 5 * 60_000,
  })
  // Account-level monthly usage — shown next to the device's own cap in
  // the Quota KPI so the user sees both "this device this month" and
  // "all my devices this month" against their account quota. Shared
  // query key with the dashboard so the cache is reused.
  const usageQ = useQuery({
    queryKey: ["me", "usage"],
    queryFn: myUsage,
    refetchInterval: 60_000,
  })

  useBreadcrumbOverride(deviceQ.data?.name)

  const live = useLiveStats((s) => s.devices[id])
  // History hydration so the chart shows real context after a refresh.
  useHistoryHydration({ deviceIds: id ? [id] : [], windowSec: 1800 })

  // Activity timeline: lifecycle, config, key, and online/offline
  // transitions. Polled every 30s so a transition emitted by the worker
  // surfaces without a manual refresh.
  // Inline panel shows just the 10 most recent entries; the rest live behind
  // the "Full view" sheet (infinite scroll). Fetching 10 also tells us whether
  // there's more to page through (length === 10 ⇒ probably more).
  const eventsQ = useQuery({
    queryKey: ["device", id, "events"],
    queryFn: () => listDeviceEvents(id, { limit: ACTIVITY_PREVIEW_COUNT }),
    enabled: id.length > 0,
    refetchInterval: 30_000,
  })

  // Cumulative RX/TX totals — the persisted figure from the device query,
  // grown live by the WS byte deltas streamed since the last refetch so the
  // headline tiles tick up in real time instead of only jumping every 20 s.
  // Called before the loading early-return below to keep hook order stable.
  const { rx: totalRx, tx: totalTx } = useLiveTotal(
    id,
    deviceQ.data?.total_rx_bytes ?? 0,
    deviceQ.data?.total_tx_bytes ?? 0
  )
  // Effective connectivity — handshake window refined by keepalive activity so
  // the pill flips offline in ~90s (like the cards) instead of waiting out the
  // ~3-min handshake window. Called before the early-return to keep hook order
  // stable. See useDeviceOnline.
  const isOnline = useDeviceOnline(deviceQ.data)

  // OHLC bandwidth candles are owned by the self-contained <CandleChart> below
  // (timeframe + zoom/pan + lazy history live inside it).

  const [revokeOpen, setRevokeOpen] = useState(false)
  // Pause is reversible but still kicks the live tunnel for the user, so
  // we gate it (and unpause) behind explicit confirms — same pattern the
  // devices list uses.
  const [pauseOpen, setPauseOpen] = useState(false)
  const [unpauseOpen, setUnpauseOpen] = useState(false)
  // Peer-configuration dialog: per-OS install tabs + config + QR + re-issue.
  // Replaces the inline wg-conf + QR panels from the previous layout.
  const [configOpen, setConfigOpen] = useState(false)
  // Key rotation: confirm → mutate → show the rotated config + QR in a
  // dialog. The result holds the only copy of the fresh private key the
  // user will see; once they dismiss the dialog it's gone.
  const [rotateConfirmOpen, setRotateConfirmOpen] = useState(false)
  const [rotated, setRotated] = useState<CreatedDevice | null>(null)
  // Edit-metadata dialog (name / OS / device type). Gated behind the
  // pencil button in the page header; the shared dialog seeds + saves
  // itself, so this page only owns the open flag.
  const [renameOpen, setRenameOpen] = useState(false)
  // Full activity log — paginated, infinite-scroll side sheet.
  const [activityOpen, setActivityOpen] = useState(false)
  // Minute-granularity clock for the "added N ago" age label — render-pure
  // substitute for reading Date.now() during render.
  const nowMs = useNow(60_000)

  const pauseM = useMutation({
    mutationFn: () => pauseDevice(id),
    onSuccess: () => {
      setPauseOpen(false)
      void qc.invalidateQueries({ queryKey: ["device", id] })
      void qc.invalidateQueries({ queryKey: ["devices"] })
      toast.info("Device paused")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  const unpauseM = useMutation({
    mutationFn: () => unpauseDevice(id),
    onSuccess: () => {
      setUnpauseOpen(false)
      void qc.invalidateQueries({ queryKey: ["device", id] })
      void qc.invalidateQueries({ queryKey: ["devices"] })
      toast.success("Device active")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  const rotateM = useMutation({
    mutationFn: () => rotateDeviceKeys(id),
    onSuccess: (data) => {
      setRotateConfirmOpen(false)
      // Push the fresh config + QR into the same query cache the peer-
      // configuration dialog reads from, so the open dialog updates
      // instantly without needing a separate reveal modal.
      qc.setQueryData(["device", id, "conf"], data)
      void qc.invalidateQueries({ queryKey: ["device", id] })
      void qc.invalidateQueries({ queryKey: ["devices"] })
      // Only pop the standalone "rotated config" reveal when the user
      // triggered rotation from somewhere OTHER than the peer-config
      // dialog (e.g. via a future action). When the peer-config dialog
      // is the one open, the cache update above already does the job.
      if (!configOpen) {
        setRotated(data)
      }
      toast.success("Keys rotated — save the new config")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  // Re-download is now handled inside the PeerConfigDialog itself via a
  // `useQuery` that fires when the dialog opens — see PeerConfigDialog.
  const deleteM = useMutation({
    mutationFn: () => deleteDevice(id),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["devices"] })
      toast.warning("Device revoked")
      navigate("/app/devices")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  if (deviceQ.isLoading || !deviceQ.data) {
    return (
      <div className="flex flex-col gap-4">
        <Skeleton className="h-8 w-48 rounded-none" />
        <Skeleton className="h-32 rounded-none" />
        <Skeleton className="h-64 rounded-none" />
      </div>
    )
  }
  const d = deviceQ.data
  // Device-type icon shown before the peer name in the header — a glanceable
  // indicator of what the peer is.
  const TypeIcon = DEVICE_TYPE_ICONS[d.device_type]
  const rxHistory = live?.rxHistory ?? []
  const txHistory = live?.txHistory ?? []
  // Combined per-second total (rx+tx) — the "Total" KPI card's line chart.
  // rx/tx histories are pushed together so they share a length.
  const totalHistory = rxHistory.map((v, i) => v + (txHistory[i] ?? 0))
  const isPaused = d.status === "paused"
  const isRevoked = d.status === "revoked"

  const addedMs = nowMs - new Date(d.created_at).getTime()
  const ageLabel = fmtRel(addedMs).replace(" ago", "")

  const server = serverQ.data
  const endpoint = server
    ? `${server.endpoint_host}:${server.endpoint_port}`
    : ""
  const allowedIpsForDisplay = "0.0.0.0/0, ::/0"

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow={`${deviceTypeLabel(d.device_type)} · ${osLabel(d.os)} · ${d.id.slice(0, 8).toUpperCase()}`}
          title={
            <span className="inline-flex items-center gap-2">
              <TypeIcon
                className="size-[0.8em] shrink-0 text-muted-foreground"
                title={`${deviceTypeLabel(d.device_type)} · ${osLabel(d.os)}`}
              />
              <span className="min-w-0 break-words">{d.name}</span>
            </span>
          }
          sub={
            <span className="inline-flex flex-wrap items-center gap-x-1.5 gap-y-1">
              <span className="font-mono">{d.allocated_ip}</span>
              <CopyIcon value={d.allocated_ip} title="Copy IP address" />
              <span className="text-muted-foreground/70">
                · added {ageLabel} ago
              </span>
            </span>
          }
          right={
            <>
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
              {!isRevoked && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setRenameOpen(true)}
                >
                  <IconPencil size={14} />
                  Edit
                </Button>
              )}
              {d.status === "active" && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setPauseOpen(true)}
                  disabled={pauseM.isPending}
                >
                  <IconPlayerPause size={14} />
                  Pause
                </Button>
              )}
              {d.status === "paused" && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setUnpauseOpen(true)}
                  disabled={unpauseM.isPending}
                >
                  <IconPlayerPlay size={14} />
                  Resume
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setConfigOpen(true)}
                disabled={isRevoked}
              >
                <IconSettings size={14} />
                Config
              </Button>
              {!isRevoked && (
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={() => setRevokeOpen(true)}
                >
                  <IconTrash size={14} />
                  Revoke
                </Button>
              )}
            </>
          }
        />
      </StaggerItem>

      {/* KPI strip — design's 4-up: RX live, TX live, Total (window), Quota */}
      <StaggerItem>
        <KpiStrip>
          <Kpi
            label="RX"
            value={<span className="tabular-nums">{formatBytes(totalRx)}</span>}
            spark={isOnline ? rxHistory.slice(-32) : []}
            sparkColor="var(--chart-1)"
            footL={isOnline ? formatBps(live?.rxBps ?? 0) : "idle"}
            footR={isOnline ? "live" : undefined}
            deltaTone={isOnline ? "up" : undefined}
          />
          <Kpi
            label="TX"
            value={<span className="tabular-nums">{formatBytes(totalTx)}</span>}
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
            accountUsed={usageQ.data?.current_month_bytes ?? null}
            accountCap={usageQ.data?.monthly_byte_cap ?? null}
          />
        </KpiStrip>
      </StaggerItem>

      {/* Bandwidth — full width now that Configuration lives in the header
          "Edit" dialog (peer config + QR stay in the "Config" dialog). The
          chart owns its timeframe + zoom/pan controls. */}
      <StaggerItem>
        <CandleChart
          scope="device"
          id={id}
          height={260}
          title="Bandwidth"
          sub="scroll to zoom · drag to pan"
        />
      </StaggerItem>


      {/* Activity timeline — lifecycle, online/offline transitions, and
          every config and key change recorded against this device.
          Powered by the audit_logs table; updated lazily so an event the
          worker writes mid-session appears within ~30 s. */}
      <StaggerItem>
        <Panel
          title="Activity"
          sub="Lifecycle, connectivity and configuration changes"
          right={
            <div className="flex items-center gap-1.5">
              <Button
                size="sm"
                variant="ghost"
                onClick={() => eventsQ.refetch()}
                disabled={eventsQ.isFetching}
                title="Refresh"
              >
                <IconRefresh
                  size={12}
                  className={eventsQ.isFetching ? "animate-spin" : undefined}
                />
                Refresh
              </Button>
              {/* Surfaced whenever there's any activity — the sheet is the
                comfortable, paginated reading view even when the inline
                preview already shows everything. */}
              {(eventsQ.data?.length ?? 0) > 0 && (
                <Button size="sm" onClick={() => setActivityOpen(true)}>
                  <IconListDetails size={12} />
                  Full view
                </Button>
              )}
            </div>
          }
        >
          <DeviceTimeline
            events={eventsQ.data ?? []}
            loading={eventsQ.isLoading}
            error={eventsQ.error}
            onRetry={() => eventsQ.refetch()}
          />
        </Panel>
      </StaggerItem>

      <ConfirmDialog
        open={revokeOpen}
        onOpenChange={setRevokeOpen}
        title={`Revoke ${d.name}?`}
        description="This removes the peer from WireGuard, frees its IP, and is irreversible. The user must add a new device to reconnect."
        confirmLabel="Revoke"
        destructive
        confirmText={d.name}
        pending={deleteM.isPending}
        onConfirm={() => deleteM.mutate()}
      />

      <ConfirmDialog
        open={pauseOpen}
        onOpenChange={setPauseOpen}
        title="Pause device?"
        description="The peer is removed from WireGuard until you resume it. The device's IP is held — no traffic is tunnelled while paused."
        confirmLabel="Pause"
        pending={pauseM.isPending}
        onConfirm={() => pauseM.mutate()}
      />

      <ConfirmDialog
        open={unpauseOpen}
        onOpenChange={setUnpauseOpen}
        title="Resume device?"
        description="The peer is re-added to WireGuard with its previously allocated IP. Traffic will start tunnelling as soon as the device handshakes."
        confirmLabel="Resume"
        pending={unpauseM.isPending}
        onConfirm={() => unpauseM.mutate()}
      />

      {/* Peer configuration dialog — per-OS install tabs with copy /
          download / re-issue affordances. Triggered by the page-head
          "Config" button. */}
      <PeerConfigDialog
        open={configOpen}
        onOpenChange={setConfigOpen}
        deviceId={d.id}
        peerName={d.name}
        defaultOs={d.os}
        placeholderConfig={renderWgConf({
          device: d,
          allowedIpsForDisplay,
          endpoint,
        })}
        onReissue={() => {
          // Keep the peer-config dialog open behind the confirm — when
          // the user confirms, rotateM.onSuccess writes the fresh config
          // straight into the dialog's query cache so it updates in
          // place. The confirm dialog stacks on top via portal z-order.
          setRotateConfirmOpen(true)
        }}
        reissuing={rotateM.isPending}
      />


      <ConfirmDialog
        open={rotateConfirmOpen}
        onOpenChange={setRotateConfirmOpen}
        title="Re-issue keys?"
        description="A fresh keypair is generated server-side. The current .conf on your device stops working immediately — you'll need to import the new one or re-scan the QR."
        confirmLabel="Re-issue"
        pending={rotateM.isPending}
        onConfirm={() => rotateM.mutate()}
      />

      {/* Name / OS / device type — purely presentational metadata, so no
          tunnel impact. Shared with the list + grid 3-dot menus. */}
      <EditDeviceDialog
        device={deviceQ.data ?? null}
        open={renameOpen}
        onOpenChange={setRenameOpen}
      />

      {/* Full activity log — infinite-scroll side sheet behind "Full view". */}
      <ActivitySheet
        deviceId={id}
        open={activityOpen}
        onOpenChange={setActivityOpen}
      />

      {/* Rotated-config reveal: shows the new QR + wg-conf in the same
          shape as the create-device dialog's step 2. The private key only
          exists in this payload — once the user dismisses, it's gone. */}
      <Dialog
        open={!!rotated}
        onOpenChange={(open) => {
          if (!open) setRotated(null)
        }}
      >
        <DialogContent className="sm:max-w-[640px]">
          <DialogHeader>
            <DialogTitle>
              <Eyebrow>New config · save it now</Eyebrow>
            </DialogTitle>
            <DialogDescription>
              The fresh private key is in this config and{" "}
              <strong className="text-foreground">isn't stored</strong> after
              you dismiss this dialog.
            </DialogDescription>
          </DialogHeader>
          {rotated && (
            <div className="space-y-4">
              <div className="grid gap-0 border border-border md:grid-cols-[auto_1fr]">
                <div className="flex aspect-square shrink-0 items-center justify-center border-border bg-card p-3 md:aspect-auto md:w-[300px] md:border-r">
                  <span
                    className="block size-[256px] max-w-full [&>svg]:size-full"
                    dangerouslySetInnerHTML={{ __html: rotated.qr_svg }}
                  />
                </div>
                <div className="flex min-w-0 flex-col gap-3 p-4">
                  <Eyebrow>Scan with WireGuard / mobile</Eyebrow>
                  <p className="font-mono text-[11px] text-muted-foreground">
                    Allocated IP{" "}
                    <span className="text-foreground">
                      {rotated.device.allocated_ip}
                    </span>
                  </p>
                  <div className="mt-auto grid grid-cols-2 gap-2">
                    <Button
                      size="sm"
                      onClick={() =>
                        downloadConfig(serverQ.data?.name, rotated.config)
                      }
                    >
                      <IconDownload size={14} />
                      Download .conf
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        if (copyText(rotated.config))
                          toast.success("Config copied")
                        else toast.error("Failed to copy")
                      }}
                    >
                      Copy config
                    </Button>
                  </div>
                </div>
              </div>
              <div className="max-h-[260px] overflow-y-auto">
                <CopyableCode value={rotated.config} multiline />
              </div>
              <DialogFooter>
                <Button onClick={() => setRotated(null)}>Done</Button>
              </DialogFooter>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </PageStagger>
  )
}

/** Full activity log in a right-side sheet. Keyset-paginated via
 *  `useInfiniteQuery` (cursor = last row's id) and auto-loads the next page
 *  when a sentinel near the bottom scrolls into view, so the admin can scroll
 *  through the device's entire history without manual "load more" clicks.
 *  Reuses <DeviceTimeline> to render the accumulated pages, so day grouping
 *  and online/offline durations span the whole list. Only fetches while open. */
function ActivitySheet({
  deviceId,
  open,
  onOpenChange,
}: {
  deviceId: string
  open: boolean
  onOpenChange: (v: boolean) => void
}) {
  const q = useInfiniteQuery({
    queryKey: ["device", deviceId, "events", "all"],
    queryFn: ({ pageParam }) =>
      listDeviceEvents(deviceId, {
        limit: ACTIVITY_PAGE_SIZE,
        beforeId: pageParam,
      }),
    initialPageParam: undefined as number | undefined,
    // A short final page (fewer than a full page) means we've hit the end;
    // otherwise the cursor is the oldest id we just received.
    getNextPageParam: (last) =>
      last.length === ACTIVITY_PAGE_SIZE ? last[last.length - 1].id : undefined,
    enabled: open,
  })

  const events = useMemo(() => q.data?.pages.flat() ?? [], [q.data])

  const scrollRef = useRef<HTMLDivElement | null>(null)
  const sentinelRef = useRef<HTMLDivElement | null>(null)

  const { hasNextPage, isFetchingNextPage, fetchNextPage } = q
  const loadMore = useCallback(() => {
    if (hasNextPage && !isFetchingNextPage) void fetchNextPage()
  }, [hasNextPage, isFetchingNextPage, fetchNextPage])

  // Observe a sentinel just below the list; when it enters the scroll
  // viewport (200px early) pull the next page. `events.length` is a dep so the
  // observer re-arms after each page lands — if the new content still doesn't
  // fill the viewport, it fires again until it does or the list is exhausted.
  useEffect(() => {
    if (!open) return
    const el = sentinelRef.current
    if (!el) return
    const io = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) loadMore()
      },
      { root: scrollRef.current, rootMargin: "200px" }
    )
    io.observe(el)
    return () => io.disconnect()
  }, [open, loadMore, events.length])

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        className="flex flex-col gap-0 p-0 data-[side=right]:w-full data-[side=right]:sm:max-w-lg"
      >
        <SheetHeader className="border-b border-border p-4">
          <SheetTitle>
            <Eyebrow>Activity log</Eyebrow>
          </SheetTitle>
          <SheetDescription>
            Every lifecycle, connectivity and configuration change recorded for
            this device, newest first.
          </SheetDescription>
        </SheetHeader>

        <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto p-4">
          <DeviceTimeline
            events={events}
            loading={q.isLoading}
            error={q.error}
            onRetry={() => void q.refetch()}
          />
          {/* Sentinel + paging status. Kept inside the scroll area so the
              IntersectionObserver root (the scroll container) sees it. */}
          <div ref={sentinelRef} aria-hidden className="h-px" />
          {isFetchingNextPage && (
            <p className="py-3 text-center font-mono text-[11px] text-muted-foreground/70">
              Loading more…
            </p>
          )}
          {!hasNextPage && !q.isLoading && events.length > 0 && (
            <p className="py-3 text-center font-mono text-[11px] text-muted-foreground/50">
              End of activity
            </p>
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}

/** "Quota" KPI card — device and account usage side-by-side inside one
 *  KPI cell so the card height matches the other tiles (RX / TX /
 *  Total) instead of stacking and visually dominating the strip.
 *
 *  Each half renders a compact mini block: small label, used number,
 *  progress bar, and a one-line footer. Both bars share the same green /
 *  amber / red bucket (≥70% / ≥90%). The auto-pause badge sits in the
 *  device half's footer since the device — not the account — is what
 *  actually gets paused. */
function QuotaKpi({
  used,
  cap,
  autoPaused,
  accountUsed,
  accountCap,
}: {
  used: number
  cap: number | null
  autoPaused: boolean
  accountUsed: number | null
  accountCap: number | null
}) {
  const showAccount = accountUsed !== null
  return (
    <div className="zv-kpi">
      <div className="zv-kpi-label">
        <span>Quota</span>
      </div>
      <div className="mt-1 flex min-w-0 items-stretch gap-3">
        <QuotaHalf
          label="Device"
          used={used}
          cap={cap ?? 0}
          autoPaused={autoPaused}
        />
        {showAccount && (
          <>
            <div className="border-l border-border/40" />
            <QuotaHalf
              label="Account"
              used={accountUsed ?? 0}
              cap={accountCap ?? 0}
            />
          </>
        )}
      </div>
    </div>
  )
}

/** One side of the Quota KPI — used in a 2-up flex row so device and
 *  account info live in the same vertical band. `min-w-0` so the
 *  numbers can truncate via `tabular-nums` instead of forcing the row
 *  wider than the KPI cell. */
function QuotaHalf({
  label,
  used,
  cap,
  autoPaused,
}: {
  label: string
  used: number
  cap: number
  autoPaused?: boolean
}) {
  const hasCap = cap > 0
  const pct = hasCap ? Math.min(100, Math.round((used / cap) * 100)) : 0
  const tone =
    pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-500" : "bg-emerald-500"
  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="font-mono text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
        {label}
      </div>
      <div className="mt-0.5 truncate font-heading text-lg leading-tight text-foreground tabular-nums">
        {formatBytes(used)}
      </div>
      <div className="mt-1 h-1 w-full overflow-hidden bg-muted">
        {hasCap && (
          <div className={`h-full ${tone}`} style={{ width: `${pct}%` }} />
        )}
      </div>
      <div className="mt-1 flex items-baseline justify-between font-mono text-[10px] text-muted-foreground">
        <span className="truncate">
          {hasCap ? `/ ${formatBytes(cap)}` : "no cap"}
        </span>
        <span
          className={
            autoPaused ? "text-amber-600 dark:text-amber-400" : undefined
          }
        >
          {autoPaused ? "paused" : hasCap ? `${pct}%` : ""}
        </span>
      </div>
    </div>
  )
}

/** Tiny inline copy affordance: square hairline button that flips to a
 *  green check for ~1.2 s after copy. Used on every read-only config row
 *  so the user can grab any single value without clicking through to the
 *  full wg-conf dialog. */
function CopyIcon({ value, title }: { value: string; title?: string }) {
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    if (!copied) return
    const t = setTimeout(() => setCopied(false), 1200)
    return () => clearTimeout(t)
  }, [copied])
  return (
    <WithTooltip label={copied ? "Copied" : (title ?? "Copy")}>
      <button
        type="button"
        onClick={() => {
          if (copyText(value)) setCopied(true)
          else toast.error("Failed to copy")
        }}
        className="flex size-6 shrink-0 items-center justify-center border border-border text-muted-foreground transition-colors hover:border-foreground/40 hover:text-foreground"
        aria-label={title ?? "Copy value"}
      >
        {copied ? (
          <IconCheck size={12} className="text-status-online" />
        ) : (
          <IconCopy size={12} />
        )}
      </button>
    </WithTooltip>
  )
}


/** Per-action visual descriptor for the timeline. The tone tier maps to
 *  the StatusPill colour scheme so the activity feed reads at a glance
 *  ("green dot = went online", "red dot = revoked"). */
type EventTone = "online" | "offline" | "warn" | "info" | "neutral"

interface ActionSpec {
  label: string
  tone: EventTone
  icon: React.ComponentType<{ size?: number; className?: string }>
}

const ACTION_CATALOG: Record<string, ActionSpec> = {
  "device.created": {
    label: "Device created",
    tone: "info",
    icon: IconSparkles,
  },
  "device.online": {
    label: "Came online",
    tone: "online",
    icon: IconPlugConnected,
  },
  "device.reconnected": {
    label: "Reconnected",
    tone: "online",
    icon: IconPlugConnected,
  },
  "device.offline": {
    label: "Went offline",
    tone: "offline",
    icon: IconPlugConnectedX,
  },
  "device.paused": {
    label: "Paused",
    tone: "warn",
    icon: IconPlayerPause,
  },
  "device.unpaused": {
    label: "Resumed",
    tone: "online",
    icon: IconPlayerPlay,
  },
  "device.revoked": {
    label: "Revoked",
    tone: "warn",
    icon: IconTrash,
  },
  "device.updated": {
    label: "Settings updated",
    tone: "info",
    icon: IconSettings,
  },
  "device.keys_rotated": {
    label: "Keys rotated",
    tone: "info",
    icon: IconKey,
  },
  "device.conf_redownloaded": {
    label: "Config re-downloaded",
    tone: "neutral",
    icon: IconDownload,
  },
  "device.stored_key_cleared": {
    label: "Stored private key cleared",
    tone: "warn",
    icon: IconKey,
  },
  "device.reordered": {
    label: "Re-ordered in list",
    tone: "neutral",
    icon: IconSettings,
  },
}

function actionSpec(action: string): ActionSpec {
  return (
    ACTION_CATALOG[action] ?? {
      label: action,
      tone: "neutral",
      icon: IconFingerprint,
    }
  )
}

const TONE_DOT: Record<EventTone, string> = {
  online: "bg-status-online",
  offline: "bg-foreground/40",
  warn: "bg-status-degraded",
  info: "bg-primary",
  neutral: "bg-muted-foreground",
}
const TONE_RING: Record<EventTone, string> = {
  online: "ring-status-online/30",
  offline: "ring-foreground/20",
  warn: "ring-status-degraded/30",
  info: "ring-primary/30",
  neutral: "ring-muted-foreground/30",
}
const TONE_ICON: Record<EventTone, string> = {
  online: "text-status-online",
  offline: "text-muted-foreground",
  warn: "text-status-degraded",
  info: "text-primary",
  neutral: "text-muted-foreground",
}

/** Vertical timeline of device events. Renders newest-first with a left
 *  rail and an icon-on-dot per row. Each row's right-hand block shows
 *  the action label, a relative timestamp ("3 min ago") that lives-
 *  updates via <RelativeTime>, and — for online/offline transitions —
 *  a small "after Xh of being online/offline" duration computed from
 *  the previous adjacent transition. Metadata (when present) is
 *  inline-collapsible so users can dig into config diffs without
 *  drowning the feed. */
function DeviceTimeline({
  events,
  loading,
  error,
  onRetry,
}: {
  events: DeviceEvent[]
  loading: boolean
  error: unknown
  onRetry?: () => void
}) {
  if (loading) {
    return (
      <div className="flex flex-col gap-3">
        {[0, 1, 2].map((i) => (
          <Skeleton key={i} className="h-12 rounded-none" />
        ))}
      </div>
    )
  }
  if (error) {
    // Surface the real reason from ApiError when we have it (status +
    // message). Without this the user just sees "Failed to load" and
    // has to dig into devtools — pointless friction when the message
    // usually tells you exactly what's wrong (404 means the API binary
    // doesn't have the /events route yet, 500 means a query failure,
    // 401 means session expired, etc.).
    const e = error as { status?: number; message?: string } | null
    const status = e?.status
    const message = e?.message ?? "Failed to load activity."
    return (
      <div className="flex flex-col gap-2 font-mono text-[12px]">
        <p className="text-destructive">
          {status ? `${status} · ${message}` : message}
        </p>
        {status === 404 && (
          <p className="text-[11px] text-muted-foreground/80">
            The /devices/{`{id}`}/events endpoint isn't available on this API
            build — rebuild and restart the api binary (cargo run -p
            crimetracker-vpn-api) to pick it up.
          </p>
        )}
        {onRetry && (
          <Button
            size="sm"
            variant="outline"
            onClick={onRetry}
            className="self-start"
          >
            <IconRefresh size={12} />
            Retry
          </Button>
        )}
      </div>
    )
  }
  if (events.length === 0) {
    return (
      <p className="font-mono text-[11px] text-muted-foreground/80">
        No activity recorded yet. Events will appear here as the device is used
        and updated.
      </p>
    )
  }

  // Precompute durations between adjacent online/offline transitions so
  // each row can show "after Nh of being online" / "after Nh of being
  // offline". Walk newest-first but the duration belongs to the SHORTER
  // interval ending at the current row — i.e. the time since the
  // previous-state-defining transition immediately above it (which is
  // the next index in the desc-sorted list).
  const durations = computeTransitionDurations(events)

  return (
    <ol className="relative flex flex-col">
      {/* The vertical rail spans the full timeline. Each row positions
          its dot precisely on top so the line passes through. */}
      <span
        aria-hidden
        className="absolute top-2 bottom-2 left-3 border-l border-border"
      />
      {events.map((ev, i) => {
        const spec = actionSpec(ev.action)
        const Icon = spec.icon
        // Date label above the first event of a new YYYY-MM-DD (list is
        // desc-sorted, so compare against the previous row).
        const day = ev.created_at.slice(0, 10)
        const showDay = i === 0 || day !== events[i - 1].created_at.slice(0, 10)
        const dur = durations[i]
        return (
          <li key={ev.id} className="flex flex-col">
            {showDay && (
              <div className="mt-3 mb-1 ml-9 first:mt-0">
                <span className="font-mono text-[10px] tracking-[0.08em] text-muted-foreground/70 uppercase">
                  {formatDayLabel(day)}
                </span>
              </div>
            )}
            <div className="relative flex items-start gap-3 py-1.5">
              <span
                className={cn(
                  "relative mt-0.5 inline-flex size-6 shrink-0 items-center justify-center rounded-full ring-4",
                  TONE_RING[spec.tone],
                  "bg-background"
                )}
              >
                <span
                  className={cn(
                    "absolute inset-1 rounded-full",
                    TONE_DOT[spec.tone],
                    "opacity-15"
                  )}
                />
                <Icon
                  size={12}
                  className={cn("relative", TONE_ICON[spec.tone])}
                />
              </span>
              <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                  <span className="text-[13px] font-medium text-foreground">
                    {spec.label}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground tabular-nums">
                    <RelativeTime value={ev.created_at} />
                  </span>
                </div>
                <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground/80">
                  {dur && (
                    <span>
                      after {formatDuration(dur.seconds)} {dur.state}
                    </span>
                  )}
                  <EventMetadata action={ev.action} metadata={ev.metadata} />
                </div>
              </div>
            </div>
          </li>
        )
      })}
    </ol>
  )
}

/** Renders the non-trivial parts of an event's metadata blob. Only
 *  certain actions carry information worth surfacing inline — for the
 *  rest we render nothing rather than dumping raw JSON. */
function EventMetadata({
  action,
  metadata,
}: {
  action: string
  metadata: Record<string, unknown>
}) {
  const [open, setOpen] = useState(false)
  if (action === "device.paused" || action === "device.unpaused") {
    const to = metadata.to
    if (typeof to === "string") return <span>now {to}</span>
    return null
  }
  if (action === "device.updated") {
    // Free-form patch payload — collapse to a "details" toggle so the
    // row stays scannable but full data is one click away.
    const hasContent = Object.keys(metadata).length > 0
    if (!hasContent) return null
    return (
      <>
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="inline-flex items-center gap-1 underline-offset-2 hover:text-foreground hover:underline"
        >
          {open ? <IconChevronUp size={11} /> : <IconChevronDown size={11} />}
          details
        </button>
        {open && (
          <pre className="mt-1 max-h-40 w-full overflow-auto border border-border bg-muted/30 p-2 font-mono text-[10px] leading-relaxed whitespace-pre-wrap">
            {JSON.stringify(metadata, null, 2)}
          </pre>
        )}
      </>
    )
  }
  return null
}

/** For each event (in the desc-sorted timeline), compute the duration
 *  the device was in its previous state if and only if both adjacent
 *  events are online/offline transitions. Returns `{ seconds, state }`
 *  where `state` is the value of the *previous* state — i.e. how long
 *  the device was online before going offline, or vice versa. */
function computeTransitionDurations(
  events: DeviceEvent[]
): ({ seconds: number; state: "online" | "offline" } | null)[] {
  const out: ({ seconds: number; state: "online" | "offline" } | null)[] = []
  for (let i = 0; i < events.length; i++) {
    const ev = events[i]
    // The "previous" transition in chronological order is the next
    // entry in the desc-sorted list.
    const prev = events[i + 1]
    if (!prev) {
      out.push(null)
      continue
    }
    const isTransition = (a: string) =>
      a === "device.online" || a === "device.offline"
    if (!isTransition(ev.action) || !isTransition(prev.action)) {
      out.push(null)
      continue
    }
    const seconds =
      (new Date(ev.created_at).getTime() -
        new Date(prev.created_at).getTime()) /
      1000
    if (!Number.isFinite(seconds) || seconds <= 0) {
      out.push(null)
      continue
    }
    // State during the gap = the state RECORDED by `prev`. If prev was
    // "device.online" it means the device became online at prev's time
    // and stayed there until going offline at ev's time.
    const state: "online" | "offline" =
      prev.action === "device.online" ? "online" : "offline"
    out.push({ seconds, state })
  }
  return out
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`
  const m = Math.round(seconds / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  const remM = m % 60
  if (h < 24) return remM > 0 ? `${h}h ${remM}m` : `${h}h`
  const d = Math.floor(h / 24)
  const remH = h % 24
  return remH > 0 ? `${d}d ${remH}h` : `${d}d`
}

function formatDayLabel(day: string): string {
  // `day` is YYYY-MM-DD. Compare against today / yesterday so the rail
  // shows friendly day chips rather than raw dates at the top.
  const today = new Date()
  const yToday = today.toISOString().slice(0, 10)
  const yesterday = new Date(today.getTime() - 24 * 60 * 60 * 1000)
    .toISOString()
    .slice(0, 10)
  if (day === yToday) return "Today"
  if (day === yesterday) return "Yesterday"
  return formatDate(day)
}

/** Build a WireGuard config matching the design's wg-conf preview. The
 *  PrivateKey line is a placeholder because we don't persist private keys
 *  server-side — the real one was handed off at device creation. The
 *  server-side public key isn't currently exposed through /me/server
 *  either, so we show "(server public key)" until the API surfaces it. */
/** Peer-configuration dialog. Mirrors the layout from
 *  VPN-FRONTEND/PeerDetails — per-OS tabs (Linux / Windows / macOS /
 *  Android / iOS) with install hints, the rendered wg-conf in a copyable
 *  multiline block, and actions (Download / Copy / Re-issue keys, plus
 *  Re-download when the server still has the private key encrypted). */
function PeerConfigDialog({
  open,
  onOpenChange,
  deviceId,
  peerName,
  defaultOs,
  placeholderConfig,
  onReissue,
  reissuing,
}: {
  open: boolean
  onOpenChange: (v: boolean) => void
  deviceId: string
  peerName: string
  defaultOs: DeviceOs
  /** Fallback wg-conf rendered from public fields, shown only while the
   *  real server-stored config is still loading. */
  placeholderConfig: string
  onReissue: () => void
  reissuing: boolean
}) {
  const initialTab = osToTab(defaultOs)
  const serverQ = useQuery({ queryKey: ["me", "server"], queryFn: meServer, staleTime: 5 * 60_000 })

  // The server always holds an encrypted copy of the private key, so fetch
  // the real .conf (+ QR) whenever the dialog opens. Gated on `open` so the
  // page load doesn't trigger it. Cached short-lived — the key only changes
  // on re-issue, which invalidates this query's cache.
  const confQ = useQuery({
    queryKey: ["device", deviceId, "conf"],
    queryFn: () => redownloadDeviceConf(deviceId),
    enabled: open,
    staleTime: 60_000,
  })

  // Effective config text + QR: real one when available. When the
  // server doesn't hold the key AND nothing has been rotated into the
  // cache yet, we have no working config to show — the dialog collapses
  // to a "re-issue to get one" call-to-action.
  const realConfig = confQ.data?.config
  const realQrSvg = confQ.data?.qr_svg
  const loadingReal = confQ.isLoading
  // The placeholder is only useful as something to download when we
  // actually have a real config; otherwise it'd just be a broken file.
  const effectiveConfig = realConfig ?? placeholderConfig
  // Has-working-config: gates the tabs + Download button. Once a
  // rotation lands its result in the cache (or the server-stored
  // private key gets fetched), this flips to true and the tabs show.
  const hasWorkingConfig = !!realConfig

  const configName = wireguardConfigName(serverQ.data?.name)
  const downloadNow = () => downloadConfig(configName, effectiveConfig)
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        className="flex flex-col gap-0 p-0 data-[side=right]:w-full data-[side=right]:sm:max-w-3xl"
      >
        <SheetHeader className="border-b border-border p-4 pr-12">
          <SheetTitle>
            <Eyebrow>
              Peer configuration ·{" "}
              <span className="text-foreground">{peerName}</span>
            </Eyebrow>
          </SheetTitle>
          <SheetDescription>
            Per-OS install steps below. The same wg-conf works for every
            platform — pick whichever has the smoothest hand-off for the device
            you're setting up.
          </SheetDescription>
        </SheetHeader>

        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-hidden p-4">
          {/* Banner — explicit about what config the user is seeing.
              Stored-key + loading: muted "loading" pill.
              Stored-key + error: destructive pill, re-issue offered.
              No stored key AND nothing rotated in yet: full call-to-action;
              the tabs + download below are hidden so the sheet reads as a
              single "re-issue to get a working config" prompt instead of
              offering a placeholder file that wouldn't actually connect. */}
          {hasWorkingConfig ? null : loadingReal ? (
            <div className="border border-border bg-muted/40 px-3 py-2 font-mono text-[11px] text-muted-foreground">
              Loading config…
            </div>
          ) : confQ.isError ? (
            <div className="border border-destructive/40 bg-destructive/5 px-3 py-2 font-mono text-[11px] text-destructive">
              Couldn't fetch the config. Re-issue keys for a fresh one.
            </div>
          ) : null}

          {hasWorkingConfig && (
            <Tabs
              defaultValue={initialTab}
              className="flex min-h-0 flex-1 flex-col"
            >
              <TabsList className="grid !h-auto grid-cols-3 gap-1 sm:!h-8 sm:grid-cols-5 sm:gap-0">
                {/* `!h-auto` overrides the TabsList variant's `h-8` (which
                pins the list to one row, squashing both rows on mobile)
                while keeping the single-row 32px height on sm+. Row gap
                gives the wrapped (Android / iOS) row breathing room. */}
                <TabsTrigger value="linux">
                  <IconTerminal2 size={14} className="mr-1.5" />
                  Linux
                </TabsTrigger>
                <TabsTrigger value="windows">
                  <IconBrandWindows size={14} className="mr-1.5" />
                  Win
                </TabsTrigger>
                <TabsTrigger value="macos">
                  <IconBrandApple size={14} className="mr-1.5" />
                  macOS
                </TabsTrigger>
                <TabsTrigger value="android">
                  <IconBrandAndroid size={14} className="mr-1.5" />
                  Android
                </TabsTrigger>
                <TabsTrigger value="ios">
                  <IconDeviceMobile size={14} className="mr-1.5" />
                  iOS
                </TabsTrigger>
              </TabsList>

              {/* Linux / Windows: ConfigBlock sits between Step rows, so we
              can't flex-grow it (it would push later steps off-screen).
              Use space-y-3 + overflow-y-auto on the panel itself so the
              tab content scrolls vertically when there are more steps
              than viewport. */}
              <TabsContent
                value="linux"
                className="mt-3 min-h-0 space-y-3 overflow-y-auto"
              >
                <Step
                  n={1}
                  text="Create the configuration file"
                  command={`sudo touch /etc/wireguard/${configName}.conf`}
                />
                <Step
                  n={2}
                  text="Open the file and paste the config below"
                  command={`sudo nano /etc/wireguard/${configName}.conf`}
                />
                <ConfigBlock value={effectiveConfig} />
                <Step
                  n={3}
                  text="Bring the tunnel up"
                  command={`sudo wg-quick up ${configName}`}
                />
                <Step
                  n={4}
                  text="Enable on boot (optional)"
                  command={`sudo systemctl enable wg-quick@${configName}`}
                />
              </TabsContent>

              <TabsContent
                value="windows"
                className="mt-3 min-h-0 space-y-3 overflow-y-auto"
              >
                <Step
                  n={1}
                  text={`Save the config below as ${configName}.conf`}
                />
                <ConfigBlock value={effectiveConfig} />
                <p className="font-mono text-[11px] leading-relaxed text-muted-foreground">
                  Step 2: open the WireGuard application → "Import tunnel(s)
                  from file" → select <strong>{peerName}.conf</strong>.
                </p>
              </TabsContent>

              {/* macOS / Android / iOS: ConfigBlock is the last child, so we
              flex-grow it via the `grow` prop. The TabsContent becomes a
              flex column so flex-1 on the block propagates. */}
              <TabsContent
                value="macos"
                className="mt-3 flex min-h-0 flex-1 flex-col gap-3"
              >
                <p className="font-mono text-[11px] leading-relaxed text-muted-foreground">
                  For the GUI app from the App Store, save the config and use
                  "Import tunnel(s) from file". For the CLI (Homebrew
                  wireguard-tools):
                </p>
                <Step
                  n={1}
                  text="Create the file"
                  command={`touch /etc/wireguard/${configName}.conf`}
                />
                <Step
                  n={2}
                  text="Paste the config and bring it up"
                  command={`sudo wg-quick up ${configName}`}
                />
                <ConfigBlock value={effectiveConfig} grow />
              </TabsContent>

              <TabsContent
                value="android"
                className="mt-3 flex min-h-0 flex-1 flex-col gap-3"
              >
                <MobileQrSlot
                  qrSvg={realQrSvg}
                  loading={loadingReal}
                  onReissue={onReissue}
                />
                <p className="font-mono text-[11px] leading-relaxed text-muted-foreground">
                  Install the WireGuard app, tap the <strong>+</strong> button →
                  "Scan from QR code" and point it at the QR above. Or use
                  "Create from file or archive" with the .conf below.
                </p>
                <ConfigBlock value={effectiveConfig} grow />
              </TabsContent>

              <TabsContent
                value="ios"
                className="mt-3 flex min-h-0 flex-1 flex-col gap-3"
              >
                <MobileQrSlot
                  qrSvg={realQrSvg}
                  loading={loadingReal}
                  onReissue={onReissue}
                />
                <p className="font-mono text-[11px] leading-relaxed text-muted-foreground">
                  Install the WireGuard app from the App Store, tap{" "}
                  <strong>+</strong> → "Create from QR code" and point the
                  camera at the QR above.
                </p>
                <ConfigBlock value={effectiveConfig} grow />
              </TabsContent>
            </Tabs>
          )}
        </div>

        <SheetFooter className="mt-0 flex flex-row flex-wrap justify-end gap-2 border-t border-border p-4">
          {/* Download is only meaningful once we have a real config. */}
          {hasWorkingConfig && (
            <Button
              size="sm"
              variant="ghost"
              onClick={downloadNow}
              disabled={loadingReal}
            >
              <IconDownload size={14} />
              Download .conf
            </Button>
          )}
          <Button
            size="sm"
            variant="outline"
            onClick={onReissue}
            disabled={reissuing}
          >
            <IconRefresh size={14} />
            {reissuing ? "Rotating…" : "Re-issue keys"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  )
}

function Step({
  n,
  text,
  command,
}: {
  n: number
  text: string
  command?: string
}) {
  return (
    <div className="flex items-start gap-3">
      <span className="flex size-5 shrink-0 items-center justify-center border border-border bg-muted font-mono text-[11px] text-foreground">
        {n}
      </span>
      <div className="min-w-0 flex-1 space-y-1">
        <p className="text-[13px] leading-snug text-foreground/90">{text}</p>
        {command && (
          <div className="flex items-center justify-between gap-2 border border-border bg-muted/40 px-2 py-1.5">
            <code className="font-mono text-[11px] break-all text-foreground/90">
              {command}
            </code>
            <CopyIcon value={command} />
          </div>
        )}
      </div>
    </div>
  )
}

function ConfigBlock({
  value,
  grow = false,
}: {
  value: string
  /** When the ConfigBlock is the last child of its tab panel (iOS /
   *  Android / macOS), flex-grow it so the wg-conf preview absorbs the
   *  remaining sheet height — otherwise admins waste a third of the
   *  sheet on empty space below the box. Tabs where the block sits
   *  between Steps (Linux / Windows) leave `grow` off and get a fixed
   *  max-h, since stretching it there would push later steps off-screen. */
  grow?: boolean
}) {
  return (
    <div
      className={cn(
        "overflow-y-auto",
        grow ? "min-h-[160px] flex-1" : "max-h-[160px] sm:max-h-[220px]"
      )}
    >
      <CopyableCode value={value} multiline />
    </div>
  )
}

/** Mobile-tab QR slot. Shows the real server-rendered QR (the private key
 *  is always stored), a loading placeholder while fetching, or a re-issue
 *  call-to-action if the fetch failed. Scoped to Android / iOS tabs —
 *  desktop platforms get the .conf file alone. */
function MobileQrSlot({
  qrSvg,
  loading,
  onReissue,
}: {
  qrSvg: string | undefined
  loading: boolean
  onReissue: () => void
}) {
  return (
    <div className="flex flex-col items-center gap-2">
      <div className="flex size-[280px] max-w-full items-center justify-center border border-border bg-card p-3 text-center font-mono text-[10px] leading-tight text-muted-foreground">
        {loading ? (
          <span>loading QR…</span>
        ) : qrSvg ? (
          <span
            className="block size-full [&>svg]:size-full"
            dangerouslySetInnerHTML={{ __html: qrSvg }}
          />
        ) : (
          <span>QR unavailable — try again</span>
        )}
      </div>
      {!loading && !qrSvg && (
        <Button size="sm" variant="outline" onClick={onReissue}>
          <IconRefresh size={14} />
          Re-issue keys
        </Button>
      )}
    </div>
  )
}

/** Pick the dialog's initial tab from the device's recorded OS so the
 *  user lands on instructions for the platform they actually have. */
function osToTab(os: DeviceOs): string {
  switch (os) {
    case "ios":
      return "ios"
    case "android":
      return "android"
    case "macos":
      return "macos"
    case "windows":
      return "windows"
    case "linux":
      return "linux"
    default:
      return "linux"
  }
}

function renderWgConf({
  device,
  allowedIpsForDisplay,
  endpoint,
}: {
  device: { allocated_ip: string }
  allowedIpsForDisplay: string
  endpoint: string
}) {
  return `[Interface]
PrivateKey = (held on device — not stored)
Address    = ${device.allocated_ip}/32

[Peer]
PublicKey            = (server public key)
AllowedIPs           = ${allowedIpsForDisplay}
Endpoint             = ${endpoint || "(server endpoint)"}
PersistentKeepalive  = ${KEEPALIVE_SECS}`
}

function wireguardConfigName(name: string | undefined) {
  return (name ?? "crimetracker-vpn").toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 15) || "wireguard"
}

function downloadConfig(name: string | undefined, config: string) {
  const safe = wireguardConfigName(name)
  const blob = new Blob([config], { type: "text/plain" })
  const url = URL.createObjectURL(blob)
  const a = document.createElement("a")
  a.href = url
  a.download = `${safe}.conf`
  a.click()
  URL.revokeObjectURL(url)
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`
  if (n < 1024 ** 4) return `${(n / 1024 ** 3).toFixed(2)} GB`
  return `${(n / 1024 ** 4).toFixed(2)} TB`
}
