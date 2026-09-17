import {
  IconArrowsMaximize,
  IconArrowsMinimize,
  IconBinaryTree2,
  IconBrandAndroid,
  IconBrandApple,
  IconBrandUbuntu,
  IconBrandWindows,
  IconCircleDotted,
  IconDeviceDesktop,
  IconDeviceLaptop,
  IconDeviceMobile,
  IconDevices,
  IconFocusCentered,
  IconMinus,
  IconPlus,
  IconServer,
  IconUser,
  type Icon,
} from "@tabler/icons-react"
import { useMutation, useQuery } from "@tanstack/react-query"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { RelativeTime } from "@/components/RelativeTime"
import { WithTooltip } from "@/components/ui/with-tooltip"
import { usePageVisible } from "@/hooks/usePageVisible"
import {
  getMyTopology,
  setMyTopology,
  type DeviceOs,
  type DeviceStatus,
  type PublicDevice,
} from "@/lib/api"
import { connState } from "@/lib/deviceState"
import { formatBytes } from "@/lib/units"

interface LiveTopologyProps {
  devices: PublicDevice[]
  rates: Map<string, { rxBps: number; txBps: number }>
  /** Label shown next to the central hub. Defaults to "vpn-server". */
  serverLabel?: string
  /** Optional hub meta line (e.g. CIDR). */
  serverMeta?: string
  /** Optional `user_id -> { label }` map so the user-tier nodes render
   *  with a human name. When a device's user_id isn't found in this
   *  map, the node falls back to "user · <first 6 of uuid>". For the
   *  current user, pass `{ [user.id]: { label: "me" } }` or similar. */
  userMap?: Map<string, { label: string }>
  /** When true, the graph is filtered to only currently-connected peers
   *  (connState === "online" — i.e. a fresh WireGuard handshake within
   *  the live-handshake window). Users with no online peers drop off
   *  the user tier so the graph shows "who's actually on the VPN right
   *  now" instead of the entire fleet. Defaults to false. */
  liveOnly?: boolean
}

// Baseline SVG-unit budget. Both dimensions are computed per-render
// from the container's aspect ratio so the viewBox always matches the
// actual painted area — `preserveAspectRatio` stays at "meet" and we
// get no letterboxing AND no stretching. Floors keep the content area
// large enough that the peer ring doesn't squash on tiny viewports.
const DEFAULT_H = 560
const DEFAULT_VB_W = 1000
// Min content-area envelope. On portrait containers (taller than wide)
// the canvas grows vertically past DEFAULT_H so the SVG fills the full
// container height; on landscape it grows horizontally past
// DEFAULT_VB_W as before.
// 2-level tree geometry. Hub at the center; user-tier nodes orbit on
// `USER_RING`; each user's devices orbit on a per-user ring of radius
// `peerRingFor(K)` around them. Both radii are now computed from the
// **counts** so the layout never overlaps no matter how many users or
// devices land on the graph (see `peerRingFor` / `userRingFor`). The
// minimum floors keep small graphs from looking sparse.
const MAX_PEERS = 24
const MAX_USERS = 12

// Halo footprints — the visual bounding-circle of each node type. Used by
// the layout to compute "how close can two nodes sit before they touch".
// Roughly matches the SVG glyph sizes below (`hub: r=24`, `peer: r=16`)
// plus a bit of label slack.
const HUB_HALO_R = 38
const PEER_HALO_R = 22

// Gap between adjacent halos. Larger = more breathing room, smaller =
// tighter graph. 18 reads as "clearly separate, not glued together".
const RING_GAP = 18

// Floors so small graphs (1 user, 1 device) don't look sparse.
const MIN_USER_RING = 130
const MIN_PEER_RING = 60

// Largest radii we'll grow to before clamping. Past these the graph
// becomes hard to read; the SVG just scales down via preserveAspectRatio
// to fit. Tuned for ~24 peers / 12 users on a 1000-unit viewBox.
// Tuned for the worst realistic mix (12 users × 6 devices each gives
// MAX_USER_RING ≳ 720). Past these ceilings the canvas just scales
// down via SVG preserveAspectRatio.
const MAX_USER_RING = 820
const MAX_PEER_RING = 200

/** Per-user sub-ring radius. Each user gets a **full circle** of K
 *  devices around them, with devices at 2π/K angular spacing. Sized so
 *  adjacent devices don't overlap.
 *
 *  Geometry: K nodes on a full ring of radius r are separated by chord
 *  `2r·sin(π/K)`. For non-overlap we need `2r·sin(π/K) >=
 *  2·(halo + halfGap)`, so `r >= (halo + halfGap) / sin(π/K)`. */
function peerRingFor(deviceCount: number): number {
  if (deviceCount <= 1) return MIN_PEER_RING
  const halo = PEER_HALO_R + RING_GAP / 2
  const r = halo / Math.max(Math.sin(Math.PI / deviceCount), 0.05)
  return Math.min(Math.max(MIN_PEER_RING, r), MAX_PEER_RING)
}

// ── Radial-view geometry ───────────────────────────────────────────────
// The radial view is a flat 2-ring layout: hub in the center, every
// **user** on an inner ring, every **device** on a single outer ring
// (devices ordered by user so each user's cluster sits roughly under
// its own user node). Edges connect hub→user and user→device exactly
// as in tree mode; only the device positions change.

const RADIAL_DEVICE_GAP = 40 // gap between user ring and device ring

/** Inner ring (users) radius for the radial view. Same chord-spacing
 *  derivation as the tree view's userRingFor but without a sub-ring
 *  width to clear — users alone, no devices clustered around them. */
function radialUserRingFor(userCount: number): number {
  if (userCount <= 1) return MIN_USER_RING
  const halo = PEER_HALO_R + RING_GAP / 2
  const r = halo / Math.sin(Math.PI / userCount)
  return Math.min(Math.max(MIN_USER_RING, r), MAX_USER_RING)
}

/** Outer ring (devices) radius for the radial view. Sized to fit every
 *  device on a single circle without overlap AND to clear the inner
 *  user ring + halos by `RADIAL_DEVICE_GAP`. */
function radialDeviceRingFor(
  deviceCount: number,
  userRing: number,
): number {
  const halo = PEER_HALO_R + RING_GAP / 2
  const clear = userRing + PEER_HALO_R + RADIAL_DEVICE_GAP + PEER_HALO_R
  if (deviceCount <= 1) return Math.max(clear, userRing + 90)
  const r = halo / Math.sin(Math.PI / deviceCount)
  return Math.max(r, clear)
}

/** Outer ring radius for the user tier. Two constraints:
 *
 *  1. Adjacent users' sub-rings (each `maxPeerRing` wide on the user-side
 *     half) must not overlap. With a true circular user ring,
 *     `2R·sin(π/N) >= 2·(maxPeerRing + halo + halfGap)`.
 *
 *  2. The inner edge of every sub-ring must clear the hub:
 *     `R - maxPeerRing >= HUB_HALO_R + RING_GAP`.
 *
 *  Because devices now fan **outward** from the user (never inward), the
 *  user ring no longer needs to oversize itself to keep sub-rings from
 *  eating the hub — the devices only ever sit on the far side of the
 *  user. The hub-clear constraint still bites for single-user graphs. */
function userRingFor(userCount: number, maxPeerRing: number): number {
  const halfWidth = maxPeerRing + PEER_HALO_R + RING_GAP / 2
  const minHubClear = maxPeerRing + HUB_HALO_R + RING_GAP
  if (userCount <= 1) return Math.max(MIN_USER_RING, minHubClear)
  const ang = Math.sin(Math.PI / userCount)
  const fromAngular = halfWidth / Math.max(ang, 0.05)
  const r = Math.max(MIN_USER_RING, fromAngular, minHubClear)
  return Math.min(r, MAX_USER_RING)
}

// Brand mark per OS (chip beside the peer glyph). A const map (member access)
// rather than a function returning a component — the latter trips the
// react-compiler "components created during render" rule even though the icon
// refs are stable. See the same pattern in lib/deviceIcons.
const OS_BRAND_ICON: Record<DeviceOs, Icon> = {
  ios: IconBrandApple,
  android: IconBrandAndroid,
  macos: IconBrandApple,
  windows: IconBrandWindows,
  linux: IconBrandUbuntu,
  other: IconDevices,
}

/** Generic form-factor glyph per OS — the centered icon inside the peer ring
 *  (the brand mark sits beside it as a chip). */
const OS_SHAPE_ICON: Record<DeviceOs, Icon> = {
  ios: IconDeviceMobile,
  android: IconDeviceMobile,
  macos: IconDeviceLaptop,
  windows: IconDeviceDesktop,
  linux: IconServer,
  other: IconDevices,
}

/** Visual tone for a peer node. Connection state (live handshake) is the
 *  source of truth — a device with no recent handshake is `idle`, never
 *  `live`, even if the store happens to hold a stale non-zero rate from
 *  before it dropped. Paused / revoked take precedence over connection
 *  because they're terminal in the lifecycle sense. */
function toneFor(status: DeviceStatus, online: boolean, hasTraffic: boolean) {
  if (status === "revoked") return "revoked"
  if (status === "paused") return "paused"
  if (!online) return "idle"
  // Connected counts as "online" even with no traffic right now, so a live
  // tunnel still draws an active edge/ring; "live" is reserved for a peer
  // actually transmitting (drives the animated flow + rate label).
  return hasTraffic ? "live" : "online"
}

/** Angle of slot `index` on a ring divided into `total` equal sectors,
 *  starting at -π/2 (north) so the first slot sits on top. Deterministic
 *  by (total, index) — no per-id jitter. Used for user placement around
 *  the hub; devices use the outward-arc helper below instead. */
function angleFor(total: number, index: number): number {
  if (total <= 0) return -Math.PI / 2
  return ((index + 0.5) / total) * Math.PI * 2 - Math.PI / 2
}

/** Angle for device `index` (of `k`) on the user's **full-circle** sub-
 *  ring. Slot 0 sits directly outward from the user (away from the hub)
 *  so the user-to-first-device edge lines up with the radial; the
 *  remaining devices wrap around the user evenly at 2π/K steps. */
function deviceAngleFor(k: number, index: number, userAngle: number): number {
  if (k <= 1) return userAngle
  return userAngle + (index / k) * Math.PI * 2
}

interface LaidOutPeer {
  device: PublicDevice
  x: number
  y: number
  tone: "live" | "online" | "idle" | "paused" | "revoked"
  rateBps: number
  /** Position of the user this device hangs off (where the edge starts). */
  userX: number
  userY: number
}

interface LaidOutUser {
  userId: string
  label: string
  x: number
  y: number
  /** How many of this user's devices are currently transmitting (online +
   *  traffic) — drives the animated flow on the hub→user edge. */
  liveCount: number
  /** How many are connected (online), regardless of traffic — drives the
   *  "active" (colored) edge + user-node ring. */
  onlineCount: number
  peerCount: number
}

const MIN_SCALE = 0.4
const MAX_SCALE = 3.5

interface View {
  tx: number
  ty: number
  scale: number
}

const INITIAL_VIEW: View = { tx: 0, ty: 0, scale: 1 }

type LayoutMode = "tree" | "radial"
const DEFAULT_LAYOUT_MODE: LayoutMode = "tree"

export function LiveTopology({
  devices,
  rates,
  serverLabel = "vpn-server",
  serverMeta,
  userMap,
  liveOnly = false,
}: LiveTopologyProps) {
  const [tick, setTick] = useState(0)
  const pageVisible = usePageVisible()
  useEffect(() => {
    // No tick while hidden — the SVG is offscreen, so re-rendering hundreds
    // of nodes every 1.2 s just bloats memory with stale React fibers.
    if (!pageVisible) return
    const id = setInterval(() => setTick((t) => (t + 1) % 2), 1200)
    return () => clearInterval(id)
  }, [pageVisible])

  const svgRef = useRef<SVGSVGElement | null>(null)
  const wrapRef = useRef<HTMLDivElement | null>(null)

  // Dynamic viewBox extent. We compute BOTH dimensions per-render from
  // the container's aspect so `vbW / vbH === containerW / containerH`.
  // With the viewBox matching container aspect, the default
  // `preserveAspectRatio="xMidYMid meet"` neither letterboxes nor
  // stretches — every SVG user-unit maps to the same number of CSS px on
  // both axes, so circles stay circles at every screen size AND the SVG
  // fills the container in both dimensions (which matters on mobile
  // portrait, where keeping H fixed used to leave huge vertical gaps).
  //
  // Strategy: pin whichever dimension is "long" to its DEFAULT, then
  // grow the other dimension to match aspect. That keeps the content
  // (per-user ring + user ring) at a sensible scale relative to the
  // canvas regardless of container shape.
  // Aspect-derived baseline viewBox — keeps the visible area matching
  // the container shape so we don't letterbox. The viewBox actually used
  // is computed below from this + the laid-out content envelope.
  const [containerAspect, setContainerAspect] = useState<number>(
    DEFAULT_VB_W / DEFAULT_H,
  )
  useEffect(() => {
    const el = wrapRef.current
    if (!el) return
    const update = () => {
      const r = el.getBoundingClientRect()
      if (r.height <= 0 || r.width <= 0) return
      setContainerAspect(r.width / r.height)
    }
    update()
    const ro = new ResizeObserver(update)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // Two layout modes the user can toggle between:
  //   - "tree"   (default): hub → user-tier → per-user device fans on
  //              outward arcs. Reads like a 2-level org chart.
  //   - "radial": hub center, *all* users on one inner ring, *all*
  //              devices on a single outer ring (ordered by user so
  //              each user's devices cluster under its own angle).
  // Persisted in localStorage so the choice survives reloads. Per-node
  // drag overrides (`nodePositions`) are still honored on top of the
  // selected layout, so dragging in either mode sticks. Declared up
  // here (before the layout-radii useMemo) because the viewBox sizing
  // and userRingRadius pick branch on it — JS TDZ otherwise.
  const [layoutMode, setLayoutMode] = useState<LayoutMode>(() =>
    loadLayoutMode(),
  )
  useEffect(() => {
    saveLayoutMode(layoutMode)
  }, [layoutMode])

  // ── Layout data flow ─────────────────────────────────────────────────
  // The chain below has to live above the pointer handlers + wheel
  // effect (which reference `vbW`/`vbH` in their deps) because of JS's
  // temporal-dead-zone — those callbacks evaluate their deps array on
  // render and can't see a `useMemo` declared further down. So we put
  // every data-flow useMemo here, from `devices` → `visible` →
  // `userGroups` → `layoutRadii` → `vbW/vbH` → `HUB_Y`.

  const visible = useMemo(() => {
    // Revoked is always excluded. `liveOnly` further trims to peers
    // that have completed a recent WG handshake — see connState in
    // lib/deviceState. Users whose entire device set is offline drop
    // off the user tier naturally because they have nothing to render.
    let live = devices.filter((d) => d.status !== "revoked")
    if (liveOnly) live = live.filter((d) => connState(d) === "online")
    return live.slice(0, MAX_PEERS)
  }, [devices, liveOnly])

  const hidden = Math.max(
    0,
    devices.filter((d) => d.status !== "revoked").length - visible.length,
  )

  // Group devices by their owning user. Each unique user_id becomes a
  // node on the inner ring; their devices cluster around them on a
  // small sub-ring.
  const userGroups = useMemo(() => {
    const groups = new Map<string, PublicDevice[]>()
    for (const d of visible) {
      const key = d.user_id
      const list = groups.get(key) ?? []
      list.push(d)
      groups.set(key, list)
    }
    // Cap user count for sanity in admin-wide views; the rest drop off the
    // graph but the HUD count still reflects them.
    const entries = [...groups.entries()].slice(0, MAX_USERS)
    return entries
  }, [visible])

  // Compute ring radii for BOTH layout modes — cheap, count-based math,
  // and lets us switch modes without recomputing the userGroups.
  //
  //   tree.userRing   = inner ring users sit on (current default)
  //   tree.perUser    = per-user sub-ring (sized by that user's device count)
  //   tree.maxPeer    = largest sub-ring across all users
  //   radial.userRing = inner ring users sit on in radial mode (smaller —
  //                     no sub-rings around each user)
  //   radial.devRing  = single outer ring every device sits on
  const layoutRadii = useMemo(() => {
    const perUser: Map<string, number> = new Map()
    let maxPeer = MIN_PEER_RING
    let totalDevices = 0
    for (const [userId, userDevices] of userGroups) {
      const r = peerRingFor(userDevices.length)
      perUser.set(userId, r)
      if (r > maxPeer) maxPeer = r
      totalDevices += userDevices.length
    }
    const treeUserRing = userRingFor(userGroups.length, maxPeer)
    const radialUserRing = radialUserRingFor(userGroups.length)
    const radialDevRing = radialDeviceRingFor(totalDevices, radialUserRing)
    return {
      perUser,
      maxPeer,
      treeUserRing,
      radialUserRing,
      radialDevRing,
      totalDevices,
    }
  }, [userGroups])

  // Pick a viewBox that satisfies BOTH the container aspect (so we don't
  // letterbox) AND the laid-out content envelope (so the outermost
  // device ring isn't clipped). The envelope changes with layoutMode:
  // in tree mode the farthest node is at `userRing + maxPeer`; in
  // radial mode it's the device ring radius directly.
  const { vbW, vbH } = useMemo(() => {
    const pad = PEER_HALO_R + 60
    const radial =
      layoutMode === "tree"
        ? layoutRadii.treeUserRing + layoutRadii.maxPeer
        : layoutRadii.radialDevRing
    const need = 2 * (radial + pad)
    if (containerAspect >= 1) {
      const h = Math.max(DEFAULT_H, need)
      const w = Math.max(h * containerAspect, need)
      return {
        vbW: Math.round(w),
        vbH: Math.round(Math.max(h, w / containerAspect)),
      }
    }
    const w = Math.max(DEFAULT_VB_W, need)
    const h = Math.max(w / containerAspect, need)
    return {
      vbW: Math.round(Math.max(w, h * containerAspect)),
      vbH: Math.round(h),
    }
  }, [containerAspect, layoutRadii, layoutMode])

  // Derived hub centerline — re-computed whenever the canvas resizes so
  // the hub + user-ring stay centered as vbH grows on portrait viewports.
  const HUB_Y = vbH / 2

  const [view, setView] = useState<View>(INITIAL_VIEW)
  const dragRef = useRef<{ startX: number; startY: number; tx: number; ty: number } | null>(null)
  const [isDragging, setIsDragging] = useState(false)
  const [isFullscreen, setIsFullscreen] = useState(false)

  // Hovered device → summary tooltip. Position is in container px (the
  // card is an HTML overlay, not SVG, so it stays crisp under zoom);
  // flip flags keep it inside the canvas near the right/bottom edges.
  const [hover, setHover] = useState<{
    id: string
    x: number
    y: number
    flipX: boolean
    flipY: boolean
  } | null>(null)

  const updateHover = useCallback(
    (id: string, e: React.PointerEvent<SVGGElement>) => {
      const rect = wrapRef.current?.getBoundingClientRect()
      if (!rect) return
      const x = e.clientX - rect.left
      const y = e.clientY - rect.top
      setHover({
        id,
        x,
        y,
        flipX: x > rect.width - 280,
        flipY: y > rect.height - 240,
      })
    },
    [],
  )

  // Per-node position overrides. When the user grabs a peer (or the hub)
  // and drags it, we record the resulting (x, y) in viewBox space here.
  // Subsequent renders prefer the override to the computed ring position.
  // Keyed by device id; the hub uses the literal key "__hub__".
  //
  // Two-layer persistence:
  //   - localStorage = fast-paint cache. Lazy initializer reads on mount
  //     so the chart paints the saved layout immediately (no flicker
  //     while the server fetch is in flight).
  //   - Server (GET /me/topology) = source of truth. When the response
  //     lands and differs from local, server wins (cross-device sync).
  //   - On every change, mirror to localStorage AND fire a debounced PUT
  //     so rapid drags coalesce into one network write.
  const [nodePositions, setNodePositions] = useState<Map<string, { x: number; y: number }>>(
    () => loadNodePositions(),
  )

  // Server fetch — runs once per session, hydrates into state if it
  // diverges from the localStorage cache.
  const topologyQ = useQuery({
    queryKey: ["me", "topology"],
    queryFn: getMyTopology,
    staleTime: Infinity,
  })
  // Track whether we've already hydrated from this query response so we
  // don't overwrite an in-progress user drag with stale server state.
  const hydratedFromServerRef = useRef(false)
  useEffect(() => {
    if (!topologyQ.data || hydratedFromServerRef.current) return
    const server = topologyQ.data.positions
    const next = new Map<string, { x: number; y: number }>()
    for (const [id, pos] of Object.entries(server)) {
      next.set(id, { x: pos.x, y: pos.y })
    }
    hydratedFromServerRef.current = true
    setNodePositions(next)
    saveNodePositions(next)
  }, [topologyQ.data])

  // Debounced save: 500ms after the last change, PUT to server. Mirror to
  // localStorage on every change for the next refresh's fast-paint.
  const saveMut = useMutation({
    mutationFn: (positions: Map<string, { x: number; y: number }>) => {
      const obj: Record<string, { x: number; y: number }> = {}
      for (const [id, pos] of positions) obj[id] = pos
      return setMyTopology(obj)
    },
  })
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    saveNodePositions(nodePositions)
    // Don't fire a PUT before we've hydrated from the server — would
    // race with the initial GET and might overwrite the server copy
    // with the localStorage copy if they differ.
    if (!hydratedFromServerRef.current) return
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    saveTimerRef.current = setTimeout(() => {
      saveMut.mutate(nodePositions)
    }, 500)
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodePositions])
  // While a node-drag is in flight: which node + its starting client/view
  // anchors. Set on pointerdown of a node, cleared on pointerup. While
  // non-null, the canvas-level pan handler is short-circuited so dragging
  // a node doesn't also pan the view.
  const nodeDragRef = useRef<{
    id: string
    startVx: number
    startVy: number
    baseX: number
    baseY: number
  } | null>(null)

  // Convert client (pixel) coordinates to viewBox (SVG user) coordinates,
  // so panning/zooming math stays in the same space as our laid-out nodes
  // regardless of the rendered element size.
  const clientToView = useCallback((clientX: number, clientY: number): { x: number; y: number } => {
    const svg = svgRef.current
    if (!svg) return { x: clientX, y: clientY }
    const rect = svg.getBoundingClientRect()
    const x = ((clientX - rect.left) / rect.width) * vbW
    const y = ((clientY - rect.top) / rect.height) * vbH
    return { x, y }
  }, [vbW, vbH])

  const onPointerDown = useCallback(
    (e: React.PointerEvent<SVGSVGElement>) => {
      // Ignore right/middle clicks so we don't fight context menus.
      if (e.button !== 0) return
      // Don't start a canvas-pan if a node grabbed the pointer first.
      if (nodeDragRef.current) return
      // Suppress the browser's native drag behaviors (text selection
      // spilling into the rest of the page) for the whole drag.
      e.preventDefault()
      ;(e.target as Element).setPointerCapture?.(e.pointerId)
      const p = clientToView(e.clientX, e.clientY)
      dragRef.current = { startX: p.x, startY: p.y, tx: view.tx, ty: view.ty }
      setIsDragging(true)
    },
    [clientToView, view.tx, view.ty],
  )

  const onPointerMove = useCallback(
    (e: React.PointerEvent<SVGSVGElement>) => {
      // Node-drag has priority — convert pointer to scene coords and
      // write the new position into the overrides map.
      const nd = nodeDragRef.current
      if (nd) {
        const p = clientToView(e.clientX, e.clientY)
        // Convert from viewport space → scene space (undo the pan/zoom
        // applied by the transformed <g>). Same inverse the wheel-zoom
        // handler uses for cursor anchoring.
        const sx = (p.x - view.tx) / view.scale
        const sy = (p.y - view.ty) / view.scale
        const dx = sx - nd.startVx
        const dy = sy - nd.startVy
        const nx = nd.baseX + dx
        const ny = nd.baseY + dy
        setNodePositions((prev) => {
          const next = new Map(prev)
          next.set(nd.id, { x: nx, y: ny })
          return next
        })
        return
      }
      const d = dragRef.current
      if (!d) return
      const p = clientToView(e.clientX, e.clientY)
      setView((v) => ({
        ...v,
        tx: d.tx + (p.x - d.startX),
        ty: d.ty + (p.y - d.startY),
      }))
    },
    [clientToView, view.tx, view.ty, view.scale],
  )

  const endDrag = useCallback(() => {
    dragRef.current = null
    nodeDragRef.current = null
    setIsDragging(false)
  }, [])

  /** Begin dragging a single node. Captures the pointer on the SVG so
   *  subsequent move events still hit our canvas-level handler (which is
   *  where the node-drag branch lives) even as the cursor leaves the node
   *  shape. Stops propagation so the canvas-pan handler doesn't also fire. */
  const beginNodeDrag = useCallback(
    (
      e: React.PointerEvent<SVGGElement>,
      id: string,
      baseX: number,
      baseY: number,
    ) => {
      if (e.button !== 0) return
      e.stopPropagation()
      e.preventDefault() // no native text-selection start on node drags
      setHover(null) // dragging a node dismisses the summary tooltip
      // Node drags count as dragging too, so the cursor flips to the
      // closed-hand "grabbing" (nodes inherit the SVG's cursor class).
      setIsDragging(true)
      // Capture on the SVG, not the <g>, so onPointerMove on the SVG keeps
      // firing while we drag. Without this, capture happens on the <g>
      // and move events skip the SVG handler.
      svgRef.current?.setPointerCapture?.(e.pointerId)
      const p = clientToView(e.clientX, e.clientY)
      const sx = (p.x - view.tx) / view.scale
      const sy = (p.y - view.ty) / view.scale
      nodeDragRef.current = { id, startVx: sx, startVy: sy, baseX, baseY }
    },
    [clientToView, view.tx, view.ty, view.scale],
  )

  // Wheel zoom — keep the point under the cursor anchored. Capture native
  // wheel via a non-passive listener so we can preventDefault and avoid
  // the page scrolling while zooming the graph.
  useEffect(() => {
    const svg = svgRef.current
    if (!svg) return
    const onWheel = (e: WheelEvent) => {
      e.preventDefault()
      const rect = svg.getBoundingClientRect()
      const cx = ((e.clientX - rect.left) / rect.width) * vbW
      const cy = ((e.clientY - rect.top) / rect.height) * vbH
      setView((v) => {
        const factor = Math.exp(-e.deltaY * 0.0015)
        const nextScale = clamp(v.scale * factor, MIN_SCALE, MAX_SCALE)
        // Anchor: keep the point (cx, cy) in viewBox space fixed under the
        // cursor by adjusting tx/ty after the scale change.
        const k = nextScale / v.scale
        const tx = cx - (cx - v.tx) * k
        const ty = cy - (cy - v.ty) * k
        return { tx, ty, scale: nextScale }
      })
    }
    svg.addEventListener("wheel", onWheel, { passive: false })
    return () => svg.removeEventListener("wheel", onWheel)
  }, [vbW, vbH])

  const zoom = (factor: number) => {
    setView((v) => {
      const nextScale = clamp(v.scale * factor, MIN_SCALE, MAX_SCALE)
      // Zoom around the SVG center to keep the action predictable when the
      // user hits the +/- buttons.
      const cx = vbW / 2
      const cy = vbH / 2
      const k = nextScale / v.scale
      return {
        tx: cx - (cx - v.tx) * k,
        ty: cy - (cy - v.ty) * k,
        scale: nextScale,
      }
    })
  }

  const resetView = () => {
    setView(INITIAL_VIEW)
    setNodePositions(new Map())
  }

  // "Full view": expand the topology to cover the browser window (NOT
  // the OS-level Fullscreen API). The wrap goes fixed inset-0 with a
  // high z-index so the chrome — sidebar, top bar, page header — stays
  // around just hidden behind it. Esc exits.
  const toggleFullscreen = useCallback(() => {
    setIsFullscreen((v) => !v)
  }, [])

  useEffect(() => {
    if (!isFullscreen) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setIsFullscreen(false)
    }
    // Prevent the page underneath from scrolling while in full view.
    const prevOverflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    document.addEventListener("keydown", onKey)
    return () => {
      document.body.style.overflow = prevOverflow
      document.removeEventListener("keydown", onKey)
    }
  }, [isFullscreen])

  // `visible` / `userGroups` / `layoutRadii` / `vbW,vbH` / `HUB_Y`
  // live above the pointer handlers — see the "Layout data flow"
  // comment up there for why.

  // User-ring radius picked by mode — tree mode oversizes to keep the
  // per-user sub-rings from colliding; radial mode just spaces users
  // evenly since their devices live on a separate outer ring.
  const userRingRadius =
    layoutMode === "tree" ? layoutRadii.treeUserRing : layoutRadii.radialUserRing

  const placedUsers: LaidOutUser[] = useMemo(() => {
    const n = userGroups.length
    if (n === 0) return []
    return userGroups.map(([userId, userDevices], i) => {
      // Users sit at evenly-spaced angles around the hub — true circle,
      // starting at -π/2 (north). Single user docks at the top so the
      // 2-level tree reads top-down on the dashboard.
      const a = n === 1 ? -Math.PI / 2 : angleFor(n, i)
      const computedX = vbW / 2 + Math.cos(a) * userRingRadius
      const computedY = HUB_Y + Math.sin(a) * userRingRadius
      const override = nodePositions.get(`user-${userId}`)
      const x = override?.x ?? computedX
      const y = override?.y ?? computedY
      const label = userMap?.get(userId)?.label ?? `user · ${userId.slice(0, 6)}`
      let liveCount = 0
      let onlineCount = 0
      for (const d of userDevices) {
        if (connState(d) !== "online") continue
        onlineCount += 1
        if (
          ((rates.get(d.id)?.rxBps ?? 0) + (rates.get(d.id)?.txBps ?? 0)) > 1024
        ) {
          liveCount += 1
        }
      }
      return {
        userId,
        label,
        x,
        y,
        liveCount,
        onlineCount,
        peerCount: userDevices.length,
      }
    })
  }, [userGroups, nodePositions, vbW, HUB_Y, userMap, rates, userRingRadius])

  const placed: LaidOutPeer[] = useMemo(() => {
    if (placedUsers.length === 0) return []
    const out: LaidOutPeer[] = []
    const center = { x: vbW / 2, y: HUB_Y }
    // In radial mode, every device sits on a single outer ring shared
    // across all users. Slots are assigned in user-order so each user's
    // devices land in a contiguous arc directly outward from that user
    // — the user→device edges fan out cleanly without crossing other
    // users' clusters. `slotCursor` walks the global ring as we iterate
    // users in the same angular order they sit on the inner ring.
    let slotCursor = 0
    const totalDevices = layoutRadii.totalDevices
    placedUsers.forEach((u, userIdx) => {
      const userDevices = userGroups.find(([id]) => id === u.userId)?.[1] ?? []
      const k = userDevices.length
      const userAngle =
        placedUsers.length === 1 ? -Math.PI / 2 : angleFor(placedUsers.length, userIdx)

      // Anchor each user's cluster on its own angle. We center the k
      // device slots on the global outer-ring step that's closest to
      // the user's angle — guarantees the cluster falls under the user
      // when device counts are balanced, and stays nearby when they're not.
      const startCursor =
        totalDevices > 0
          ? slotCursor
          : 0

      userDevices.forEach((d, idx) => {
        const online = connState(d) === "online"
        const rate = online ? rates.get(d.id) : undefined
        const rateBps = (rate?.rxBps ?? 0) + (rate?.txBps ?? 0)
        const hasTraffic = rateBps > 1024
        const tone = toneFor(d.status, online, hasTraffic)

        let computedX: number
        let computedY: number
        if (layoutMode === "tree") {
          // Per-user outward arc — devices fan outward from the user.
          const peerRing = layoutRadii.perUser.get(u.userId) ?? MIN_PEER_RING
          const userTreeX = center.x + Math.cos(userAngle) * userRingRadius
          const userTreeY = center.y + Math.sin(userAngle) * userRingRadius
          const a = deviceAngleFor(k, idx, userAngle)
          computedX = userTreeX + Math.cos(a) * peerRing
          computedY = userTreeY + Math.sin(a) * peerRing
        } else {
          // Radial: single outer ring; each device gets one slot of the
          // global 2π/N step. Center the cluster on the user's angle so
          // the user→device edge is short.
          const slotStep = (Math.PI * 2) / Math.max(1, totalDevices)
          const clusterCenter = startCursor + (k - 1) / 2
          const slot = startCursor + idx
          const baseAngle = -Math.PI / 2 + clusterCenter * slotStep
          const angleShift = userAngle - baseAngle
          const a = -Math.PI / 2 + slot * slotStep + angleShift
          computedX = center.x + Math.cos(a) * layoutRadii.radialDevRing
          computedY = center.y + Math.sin(a) * layoutRadii.radialDevRing
        }

        const override = nodePositions.get(d.id)
        const x = override?.x ?? computedX
        const y = override?.y ?? computedY
        out.push({
          device: d,
          x,
          y,
          tone,
          rateBps,
          userX: u.x,
          userY: u.y,
        })
      })
      slotCursor += k
    })
    return out
  }, [
    placedUsers,
    userGroups,
    rates,
    nodePositions,
    vbW,
    HUB_Y,
    layoutRadii,
    layoutMode,
    userRingRadius,
  ])

  // Hub position can also be overridden by drag. Default to canvas center.
  const hubOverride = nodePositions.get("__hub__")
  const hubX = hubOverride?.x ?? vbW / 2
  const hubY = hubOverride?.y ?? HUB_Y

  // Inverse-scale glyph stroke widths so icons + lines don't get heavy when
  // zoomed out and don't get hair-thin when zoomed in. Capped so things stay
  // readable at extreme scales.
  const strokeScale = clamp(1 / view.scale, 0.5, 2)

  return (
    <div
      ref={wrapRef}
      className={
        // select-none: drags (pan + node) must never start a text
        // selection — SVG labels and the HUD are display-only.
        isFullscreen
          ? // "Full view": fixed-position overlay covering the window.
            // z-50 floats above the sidebar/top-bar without going over
            // toasts (which use z-[100]+). bg-background hides the page
            // chrome behind so the topology reads cleanly.
            "zv-livetopo-wrap fixed inset-0 z-50 select-none bg-background"
          : "zv-livetopo-wrap relative h-full w-full select-none"
      }
    >
      <svg
        ref={svgRef}
        className={`zv-topo zv-livetopo ${isDragging ? "cursor-grabbing" : "cursor-grab"}`}
        // Dynamic viewBox width matches the container's aspect ratio so the
        // default `xMidYMid meet` neither letterboxes (the old "fit"
        // problem) nor stretches axes (the previous `none` workaround that
        // turned circles into ovals).
        viewBox={`0 0 ${vbW} ${vbH}`}
        preserveAspectRatio="xMidYMid meet"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onPointerLeave={endDrag}
        style={{ touchAction: "none" }}
      >
        <defs>
          <pattern
            id="zv-livetopo-grid"
            width="40"
            height="40"
            patternUnits="userSpaceOnUse"
            patternTransform={`translate(${view.tx} ${view.ty}) scale(${view.scale})`}
          >
            <path
              d="M 40 0 L 0 0 0 40"
              fill="none"
              stroke="currentColor"
              strokeWidth="0.5"
            />
          </pattern>
        </defs>
        {/* grid is drawn full-bleed; its pattern transform keeps it locked
            to the scene under pan/zoom */}
        <rect
          width={vbW}
          height={vbH}
          fill="url(#zv-livetopo-grid)"
          opacity="0.55"
          className="text-border"
        />

        <g
          transform={`translate(${view.tx} ${view.ty}) scale(${view.scale})`}
          style={{ vectorEffect: "non-scaling-stroke" }}
        >
          {/* server → user edges (thicker, always "live") */}
          {placedUsers.map((u) => (
            <g key={`hu-${u.userId}`}>
              <line
                x1={hubX}
                y1={hubY}
                x2={u.x}
                y2={u.y}
                className={
                  u.onlineCount > 0 ? "zv-topo-edge-live" : "zv-topo-edge"
                }
                strokeWidth={1.1 * strokeScale}
              />
              {u.liveCount > 0 && (
                <line
                  className="zv-topo-flow"
                  x1={hubX}
                  y1={hubY}
                  x2={u.x}
                  y2={u.y}
                  strokeWidth={1.4 * strokeScale}
                  strokeDasharray="2 6"
                  style={{ animationDuration: "1.8s" }}
                />
              )}
            </g>
          ))}

          {/* user → device edges + per-device flow particles */}
          {placed.map((p) => {
            const isLive = p.tone === "live"
            const connected = p.tone === "live" || p.tone === "online"
            return (
              <g key={`e-${p.device.id}`}>
                <line
                  x1={p.userX}
                  y1={p.userY}
                  x2={p.x}
                  y2={p.y}
                  className={connected ? "zv-topo-edge-live" : "zv-topo-edge"}
                  strokeWidth={0.8 * strokeScale}
                />
                {isLive && (
                  <line
                    className="zv-topo-flow"
                    x1={p.userX}
                    y1={p.userY}
                    x2={p.x}
                    y2={p.y}
                    strokeWidth={1.2 * strokeScale}
                    strokeDasharray="2 5"
                    style={{
                      animationDuration:
                        Math.max(0.8, 2.5 - Math.log10(Math.max(1, p.rateBps) / 100) * 0.3) +
                        "s",
                    }}
                  />
                )}
              </g>
            )
          })}

          {/* peers — draggable. PeerNode no longer needs hubX (label
              anchor is now relative to the user, not the hub). */}
          {placed.map((p) => (
            <PeerNode
              key={p.device.id}
              peer={p}
              hubX={p.userX}
              onPointerDown={(e) => beginNodeDrag(e, p.device.id, p.x, p.y)}
              onHover={(e) => updateHover(p.device.id, e)}
              onHoverEnd={() => setHover(null)}
            />
          ))}

          {/* user-tier nodes — drawn on top of peer edges but under the hub */}
          {placedUsers.map((u) => (
            <UserNode
              key={u.userId}
              user={u}
              onPointerDown={(e) =>
                beginNodeDrag(e, `user-${u.userId}`, u.x, u.y)
              }
            />
          ))}

          {/* hub on top, last so it overlaps incoming edges — also draggable */}
          <HubNode
            tick={tick}
            label={serverLabel}
            meta={serverMeta}
            peerCount={placed.length}
            hidden={hidden}
            hubX={hubX}
            hubY={hubY}
            onPointerDown={(e) => beginNodeDrag(e, "__hub__", hubX, hubY)}
          />
        </g>

        {/* HUD — sits in screen-space (outside the transformed group) so
            metrics stay anchored to the corner under any zoom level */}
        <g transform={`translate(${vbW - 200}, 20)`} className="zv-topo-meta">
          <text fontSize="9">
            PEERS · {placed.filter((p) => p.tone === "live" || p.tone === "online").length}/
            {devices.filter((d) => d.status !== "revoked").length}
          </text>
          <text fontSize="9" y={12}>
            USERS · {placedUsers.length}
          </text>
          <text fontSize="9" y={24}>
            HUBS · 1
          </text>
          <text fontSize="9" y={36}>
            ZOOM · {view.scale.toFixed(2)}×
          </text>
        </g>
      </svg>

      {/* Pan/zoom controls — keep them small + tucked in the bottom-right
          so the canvas stays uncluttered. Reset clears node-drag overrides
          too, so users can always undo a layout they don't like. */}
      <div className="absolute bottom-2 right-2 flex flex-col gap-1">
        <WithTooltip
          side="left"
          label={
            layoutMode === "tree"
              ? "Switch to radial view (devices on outer ring)"
              : "Switch to tree view (devices clustered per user)"
          }
        >
          <button
            type="button"
            aria-label="Toggle layout mode"
            className="zv-icon-btn bg-card"
            onClick={() =>
              setLayoutMode((m) => (m === "tree" ? "radial" : "tree"))
            }
          >
            {layoutMode === "tree" ? (
              <IconCircleDotted size={14} />
            ) : (
              <IconBinaryTree2 size={14} />
            )}
          </button>
        </WithTooltip>
        <WithTooltip
          side="left"
          label={isFullscreen ? "Exit fullscreen" : "Open fullscreen"}
        >
          <button
            type="button"
            aria-label={isFullscreen ? "Exit fullscreen" : "Open fullscreen"}
            className="zv-icon-btn bg-card"
            onClick={toggleFullscreen}
          >
            {isFullscreen ? (
              <IconArrowsMinimize size={14} />
            ) : (
              <IconArrowsMaximize size={14} />
            )}
          </button>
        </WithTooltip>
        <WithTooltip side="left" label="Zoom in">
          <button
            type="button"
            aria-label="Zoom in"
            className="zv-icon-btn bg-card"
            onClick={() => zoom(1.25)}
            disabled={view.scale >= MAX_SCALE}
          >
            <IconPlus size={14} />
          </button>
        </WithTooltip>
        <WithTooltip side="left" label="Zoom out">
          <button
            type="button"
            aria-label="Zoom out"
            className="zv-icon-btn bg-card"
            onClick={() => zoom(0.8)}
            disabled={view.scale <= MIN_SCALE}
          >
            <IconMinus size={14} />
          </button>
        </WithTooltip>
        <WithTooltip side="left" label="Reset view + clear node positions">
          <button
            type="button"
            aria-label="Reset view"
            className="zv-icon-btn bg-card"
            onClick={resetView}
            disabled={
              view.tx === 0 &&
              view.ty === 0 &&
              view.scale === 1 &&
              nodePositions.size === 0
            }
          >
            <IconFocusCentered size={14} />
          </button>
        </WithTooltip>
      </div>

      {/* Device summary tooltip — follows the cursor over a peer node.
          HTML (not SVG) so it stays crisp at any zoom and can reuse the
          app's type/format components. pointer-events-none so it never
          steals the hover it depends on. */}
      {hover && !isDragging && (() => {
        const p = placed.find((pp) => pp.device.id === hover.id)
        if (!p) return null
        return (
          <DeviceSummaryCard
            peer={p}
            rates={rates.get(p.device.id)}
            ownerLabel={userMap?.get(p.device.user_id)?.label}
            x={hover.x}
            y={hover.y}
            flipX={hover.flipX}
            flipY={hover.flipY}
          />
        )
      })()}
    </div>
  )
}

/** Floating device-summary card for the topology hover. Fixed width so
 *  the edge-flip math in `updateHover` can assume its footprint. */
function DeviceSummaryCard({
  peer,
  rates,
  ownerLabel,
  x,
  y,
  flipX,
  flipY,
}: {
  peer: LaidOutPeer
  rates?: { rxBps: number; txBps: number }
  ownerLabel?: string
  x: number
  y: number
  flipX: boolean
  flipY: boolean
}) {
  const d = peer.device
  const online = peer.tone === "live" || peer.tone === "online"
  const statusWord =
    d.status === "paused" ? "paused" : online ? "online" : "offline"
  const statusClass =
    d.status === "paused"
      ? "text-amber-500"
      : online
        ? "text-emerald-500"
        : "text-muted-foreground"
  const rows: [string, React.ReactNode][] = []
  if (ownerLabel) rows.push(["owner", ownerLabel])
  rows.push(["system", `${d.os} · ${d.device_type}`])
  rows.push(["vpn ip", d.allocated_ip])
  if (d.last_peer_endpoint) rows.push(["endpoint", d.last_peer_endpoint])
  if (online) {
    rows.push([
      "rate",
      `↓ ${formatRateShort(rates?.rxBps ?? 0)} · ↑ ${formatRateShort(rates?.txBps ?? 0)}`,
    ])
  }
  rows.push([
    "handshake",
    <RelativeTime key="hs" value={d.last_handshake_at} fallback="never" />,
  ])
  rows.push([
    "this month",
    formatBytes(d.current_month_bytes) +
      (d.monthly_byte_cap && d.monthly_byte_cap > 0
        ? ` / ${formatBytes(d.monthly_byte_cap)}`
        : ""),
  ])
  rows.push([
    "lifetime",
    `↓ ${formatBytes(d.total_rx_bytes)} · ↑ ${formatBytes(d.total_tx_bytes)}`,
  ])
  return (
    <div
      className="border-border bg-card pointer-events-none absolute z-10 w-[248px] border p-2.5 shadow-lg"
      style={{
        left: x,
        top: y,
        transform: `translate(${flipX ? "calc(-100% - 14px)" : "14px"}, ${
          flipY ? "calc(-100% - 10px)" : "10px"
        })`,
      }}
    >
      <div className="mb-1.5 flex items-baseline justify-between gap-2">
        <span className="text-foreground truncate text-xs font-semibold">
          {d.name}
        </span>
        <span className={`font-mono text-[10px] ${statusClass}`}>
          ● {statusWord}
        </span>
      </div>
      <dl className="flex flex-col gap-0.5">
        {rows.map(([label, value]) => (
          <div
            key={label}
            className="flex items-baseline justify-between gap-3 font-mono text-[10px]"
          >
            <dt className="text-muted-foreground shrink-0 uppercase tracking-wide">
              {label}
            </dt>
            <dd className="text-foreground truncate">{value}</dd>
          </div>
        ))}
      </dl>
    </div>
  )
}

function clamp(n: number, lo: number, hi: number): number {
  return n < lo ? lo : n > hi ? hi : n
}

// ---------------------------------------------------------------------------
// Position persistence
// ---------------------------------------------------------------------------

const POS_STORAGE_KEY = "zerovpn.topology.positions.v1"

/** Read the saved drag-overrides on mount. Wrapped in try/catch because
 *  localStorage can throw in private mode / when storage is full, and we
 *  don't want a storage failure to crash the dashboard. */
function loadNodePositions(): Map<string, { x: number; y: number }> {
  if (typeof window === "undefined") return new Map()
  try {
    const raw = window.localStorage.getItem(POS_STORAGE_KEY)
    if (!raw) return new Map()
    const parsed = JSON.parse(raw) as Record<string, { x: number; y: number }>
    const map = new Map<string, { x: number; y: number }>()
    for (const [id, pos] of Object.entries(parsed)) {
      if (
        pos &&
        typeof pos.x === "number" &&
        typeof pos.y === "number" &&
        Number.isFinite(pos.x) &&
        Number.isFinite(pos.y)
      ) {
        map.set(id, { x: pos.x, y: pos.y })
      }
    }
    return map
  } catch {
    return new Map()
  }
}

/** Mirror the overrides back to localStorage. Called from a useEffect on
 *  every change so a refresh after a drag lands on the same layout. */
function saveNodePositions(positions: Map<string, { x: number; y: number }>): void {
  if (typeof window === "undefined") return
  try {
    if (positions.size === 0) {
      window.localStorage.removeItem(POS_STORAGE_KEY)
      return
    }
    const obj: Record<string, { x: number; y: number }> = {}
    for (const [id, pos] of positions) obj[id] = pos
    window.localStorage.setItem(POS_STORAGE_KEY, JSON.stringify(obj))
  } catch {
    // ignore — quota / private mode
  }
}

// ── Layout-mode persistence ───────────────────────────────────────────
//
// The tree/radial choice is purely client-side ergonomics (no server
// sync) so localStorage is enough. Wrapped in the same try/catch as
// position persistence so a quota / private-mode failure doesn't
// crash the chart.

const LAYOUT_MODE_STORAGE_KEY = "zerovpn.topology.layoutMode.v1"

function loadLayoutMode(): LayoutMode {
  if (typeof window === "undefined") return DEFAULT_LAYOUT_MODE
  try {
    const v = window.localStorage.getItem(LAYOUT_MODE_STORAGE_KEY)
    return v === "radial" || v === "tree" ? v : DEFAULT_LAYOUT_MODE
  } catch {
    return DEFAULT_LAYOUT_MODE
  }
}

function saveLayoutMode(mode: LayoutMode): void {
  if (typeof window === "undefined") return
  try {
    window.localStorage.setItem(LAYOUT_MODE_STORAGE_KEY, mode)
  } catch {
    // ignore — quota / private mode
  }
}

function HubNode({
  tick,
  label,
  meta,
  peerCount,
  hidden,
  hubX,
  hubY,
  onPointerDown,
}: {
  tick: number
  label: string
  meta?: string
  peerCount: number
  hidden: number
  hubX: number
  hubY: number
  onPointerDown: (e: React.PointerEvent<SVGGElement>) => void
}) {
  return (
    // Cursor is inherited from the SVG root (grab, or grabbing while a
    // drag — canvas or node — is in flight), so no inline cursor here.
    <g transform={`translate(${hubX}, ${hubY})`} onPointerDown={onPointerDown}>
      <circle r={28 + tick} className="zv-topo-halo" opacity="0.55" />
      <circle r={22} className="zv-topo-halo" opacity="0.85" />
      <circle r={18} className="zv-topo-hub" strokeWidth="1.4" />
      {/* server glyph */}
      <g transform="translate(-10, -10)" className="zv-topo-hub-icon">
        <IconServer size={20} strokeWidth={1.6} />
      </g>
      {/* label chip */}
      <g transform="translate(24, -10)">
        <rect width={90} height={20} className="zv-topo-hub-tag" strokeWidth="0.5" rx={1} />
        <text x={6} y={13} className="zv-topo-hub-label">
          {label}
        </text>
      </g>
      <text x={24} y={28} className="zv-topo-meta zv-topo-meta-ink">
        {peerCount} peers
        {hidden > 0 ? ` (+${hidden})` : ""}
      </text>
      {meta && (
        <text x={24} y={40} className="zv-topo-meta">
          {meta}
        </text>
      )}
    </g>
  )
}

/** Intermediate node between the server hub and a peer's devices. Sized
 *  between hub (28) and peer (14) so the visual hierarchy reads
 *  server > user > device at a glance. Tone follows whether any of the
 *  user's devices are currently transmitting. */
function UserNode({
  user,
  onPointerDown,
}: {
  user: LaidOutUser
  onPointerDown: (e: React.PointerEvent<SVGGElement>) => void
}) {
  // Connected (any device online) lights the ring; traffic isn't required.
  const strokeClass =
    user.onlineCount > 0 ? "zv-topo-peer-live" : "zv-topo-peer-idle"
  return (
    <g
      transform={`translate(${user.x}, ${user.y})`}
      className="zv-topo-peer"
      onPointerDown={onPointerDown}
    >
      <circle r={18} className={strokeClass} strokeWidth="1.4" />
      <g transform="translate(-9, -9)" className="zv-topo-peer-icon">
        <IconUser size={18} strokeWidth={1.6} />
      </g>
      <text x={0} y={32} textAnchor="middle" className="zv-topo-hub-label">
        {user.label}
      </text>
      <text x={0} y={44} textAnchor="middle" className="zv-topo-meta">
        {user.peerCount} {user.peerCount === 1 ? "device" : "devices"}
      </text>
    </g>
  )
}

function PeerNode({
  peer,
  hubX,
  onPointerDown,
  onHover,
  onHoverEnd,
}: {
  peer: LaidOutPeer
  hubX: number
  onPointerDown: (e: React.PointerEvent<SVGGElement>) => void
  /** Fired on enter AND move so the summary card tracks the cursor. */
  onHover: (e: React.PointerEvent<SVGGElement>) => void
  onHoverEnd: () => void
}) {
  const Shape = OS_SHAPE_ICON[peer.device.os]
  const Brand = OS_BRAND_ICON[peer.device.os]
  const strokeClass =
    peer.tone === "live" || peer.tone === "online"
      ? "zv-topo-peer-live"
      : peer.tone === "paused"
        ? "zv-topo-peer-paused"
        : "zv-topo-peer-idle"
  const labelAnchor: "start" | "end" = peer.x >= hubX ? "start" : "end"
  const labelDx = labelAnchor === "start" ? 18 : -18
  return (
    <g
      transform={`translate(${peer.x}, ${peer.y})`}
      className="zv-topo-peer"
      onPointerDown={onPointerDown}
      onPointerEnter={onHover}
      onPointerMove={onHover}
      onPointerLeave={onHoverEnd}
    >
      {/* outer ring */}
      <circle r={14} className={strokeClass} strokeWidth="1.2" />
      {/* OS-shape glyph at center */}
      <g transform="translate(-9, -9)" className="zv-topo-peer-icon">
        <Shape size={18} strokeWidth={1.5} />
      </g>
      {/* tiny brand chip on the bottom-right of the node */}
      <g transform="translate(8, 6)" className="zv-topo-peer-brand">
        <circle r={6} className="zv-topo-peer-brand-bg" />
        <g transform="translate(-4, -4)">
          <Brand size={8} strokeWidth={1.8} />
        </g>
      </g>
      {/* label */}
      <text
        x={labelDx}
        y={4}
        textAnchor={labelAnchor}
        className="zv-topo-peer-label"
      >
        {peer.device.name}
      </text>
      {peer.tone === "live" && peer.rateBps > 1024 && (
        <text
          x={labelDx}
          y={18}
          textAnchor={labelAnchor}
          className="zv-topo-meta"
        >
          {formatRateShort(peer.rateBps)}
        </text>
      )}
    </g>
  )
}

function formatRateShort(bps: number): string {
  if (bps < 1_000) return `${Math.round(bps)} bps`
  if (bps < 1_000_000) return `${(bps / 1_000).toFixed(0)} kbps`
  if (bps < 1_000_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`
  return `${(bps / 1_000_000_000).toFixed(1)} Gbps`
}
