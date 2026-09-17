import { useQueryClient } from "@tanstack/react-query"
import { useCallback, useEffect, useRef } from "react"

import { isPageVisible } from "@/hooks/usePageVisible"
import { useWebSocket } from "@/hooks/useWebSocket"
import type { PublicDevice } from "@/lib/api"
import { notify, osNotify } from "@/lib/notify"
import type { Event } from "@/lib/wire"
import { useEventTail } from "@/stores/eventTail"
import { useLiveStats } from "@/stores/liveStats"
import { useAuth } from "@/stores/auth"

/**
 * Mounts a single WebSocket connection for the lifetime of the
 * authenticated session and fans out events:
 *   - `stats_delta`           → liveStats store
 *   - `peer_status_changed`   → invalidate the devices query
 *   - `handshake_change`      → patch cached `last_handshake_at` so the
 *                               online pill flips instantly
 *
 * Lives inside `DashboardLayout` (above the route Outlet) so it survives
 * navigation between /app, /app/devices, /app/devices/:id, and /admin/*.
 *
 * Renders nothing — purely a side-effect host.
 */
/** Past-tense labels for device lifecycle changes, used to compose the
 *  background OS notification a user's *other* sessions get when a device is
 *  added/changed elsewhere. `reordered` is intentionally omitted — too trivial
 *  to interrupt for. */
const DEVICE_ACTION_LABEL: Record<string, string> = {
  created: "added",
  connected: "connected",
  updated: "updated",
  deleted: "removed",
  paused: "paused",
  unpaused: "resumed",
  keys_rotated: "keys rotated",
}

export function LiveStatsProvider() {
  const user = useAuth((s) => s.user)
  const qc = useQueryClient()
  const applyDelta = useLiveStats((s) => s.applyDelta)
  const applyServerSample = useLiveStats((s) => s.applyServerSample)
  const applyServerHealth = useLiveStats((s) => s.applyServerHealth)
  const pushTail = useEventTail((s) => s.push)
  // Tracks whether we skipped any deltas while the tab was hidden. When
  // the tab comes back we trigger a re-fetch of the devices query so the
  // UI catches up without us replaying the throughput history we dropped
  // (the chart re-hydrates from the historical API in the relevant pages).
  const skippedWhileHiddenRef = useRef(false)

  const onEvent = useCallback(
    (event: Event) => {
      // Hot-path events (per-tick samples) drive history arrays of up to
      // 1800 frames; cloning those at 1+ Hz while the user can't see the
      // chart is what crashes the renderer after a few hours in the
      // background. Drop them while hidden — the queries below already
      // refresh on resume, and the historical endpoint backfills any gap.
      const visible = isPageVisible()

      if (visible) {
        pushTail(event)
      }

      switch (event.type) {
        case "stats_delta":
          if (visible) {
            applyDelta(
              event.device_id,
              event.rate_rx_bps,
              event.rate_tx_bps,
              event.ts_ms,
              event.total_rx_bytes,
              event.total_tx_bytes,
            )
          } else {
            skippedWhileHiddenRef.current = true
          }
          break
        case "server_sample":
          if (visible) {
            applyServerSample(
              event.server_id,
              event.rate_rx_bps,
              event.rate_tx_bps,
              event.peer_count,
              event.online_count,
              event.handshake_count,
              event.ts_ms,
            )
          } else {
            skippedWhileHiddenRef.current = true
          }
          break
        case "peer_status_changed": {
          // Status flips are rare AND structural — always apply, the cost
          // is a single query invalidation. Lets `/devices` repaint
          // immediately when the tab comes back to find a paused peer.
          const cached = qc.getQueryData<PublicDevice[]>(["devices"])
          const dev = cached?.find((d) => d.id === event.device_id)
          const label = dev?.name ?? "A device"
          if (event.status === "revoked") {
            notify.error(`${label} was revoked`, {
              description: "All sessions for this device have been ended.",
              important: true,
              id: `dev-${event.device_id}-revoked`,
            })
          } else if (event.status === "paused") {
            notify.warning(`${label} was paused`, {
              description: "Reconnect from the device settings when ready.",
              important: true,
              id: `dev-${event.device_id}-paused`,
            })
          } else if (event.status === "active" && dev && dev.status !== "active") {
            notify.success(`${label} is active again`, {
              id: `dev-${event.device_id}-active`,
            })
          }
          void qc.invalidateQueries({ queryKey: ["devices"] })
          void qc.invalidateQueries({ queryKey: ["device", event.device_id] })
          // The admin user-detail page lists this user's devices (status +
          // per-device quota) — refresh it instantly on a pause/resume/revoke.
          void qc.invalidateQueries({
            queryKey: ["admin", "user", event.user_id],
          })
          break
        }
        case "handshake_change": {
          // A handshake just landed — the peer is connected *now*. Patch
          // the cached `last_handshake_at` directly (connState() reads
          // exactly this field) so the online pill flips on the next
          // render, with no refetch round-trip. We apply this even while
          // hidden: it's a one-field edit on a small array, and it means
          // the pill is already correct when the user returns to the tab.
          //
          // The previous version only invalidated `["device", id]`, so the
          // list views — dashboard, Devices grid, user topology — never
          // updated when a mobile peer first handshook; the pill stayed
          // "offline" until an unrelated refetch happened. We now patch
          // every cache that renders a connection pill.
          const iso = new Date(event.last_handshake_ms).toISOString()
          const patchList = (prev?: PublicDevice[]) =>
            prev?.map((d) =>
              d.id === event.device_id ? { ...d, last_handshake_at: iso } : d,
            )
          qc.setQueryData<PublicDevice[]>(["devices"], patchList)
          qc.setQueryData<PublicDevice[]>(["admin", "devices"], patchList)
          qc.setQueryData<PublicDevice>(["device", event.device_id], (prev) =>
            prev ? { ...prev, last_handshake_at: iso } : prev,
          )
          break
        }
        case "server_health":
          // Admin-only event (filtered server-side). Always apply; the
          // 5-second cadence is cheap and the sidebar panel reads from
          // this slot on every render.
          if (visible) {
            applyServerHealth(
              event.server_id,
              event.cpu_pct,
              event.mem_used_bytes,
              event.mem_total_bytes,
              event.active_peers,
              event.wg_rx_bps,
              event.wg_tx_bps,
              event.net_rx_total_bytes,
              event.net_tx_total_bytes,
              event.uptime_sec,
              event.ts_ms,
            )
          }
          break
        case "data_changed": {
          // A persisted mutation happened — on this session or another of
          // the same user, or (for admin-global resources) an admin. Refresh
          // the affected queries so add / edit / delete reflect in real time.
          // This is the cross-device sync path: a peer added on a phone shows
          // up on the laptop without a manual reload. Always applied (even
          // while hidden) — it's only cache invalidation, and it means the
          // view is already correct when the tab regains focus.
          switch (event.resource) {
            case "device": {
              // Look up the device name from cache BEFORE invalidating (the
              // cached list is still present synchronously) so a background
              // heads-up can name the device.
              const label = DEVICE_ACTION_LABEL[event.action]
              const name =
                event.id != null
                  ? qc
                      .getQueryData<PublicDevice[]>(["devices"])
                      ?.find((d) => d.id === event.id)?.name
                  : undefined
              void qc.invalidateQueries({ queryKey: ["devices"] })
              void qc.invalidateQueries({ queryKey: ["admin", "devices"] })
              void qc.invalidateQueries({ queryKey: ["me", "topology"] })
              // Topology Flows view: a new/changed peer means new possible
              // source IPs in conntrack, so refresh the connections lists.
              // The list is keyed by `["connections", ...]` (user) and
              // `["admin", "connections"]` (admin); prefix-match is fine
              // because react-query invalidates all matching subkeys.
              void qc.invalidateQueries({ queryKey: ["connections"] })
              void qc.invalidateQueries({ queryKey: ["admin", "connections"] })
              if (event.id) {
                void qc.invalidateQueries({ queryKey: ["device", event.id] })
                void qc.invalidateQueries({
                  queryKey: ["admin", "device", event.id],
                })
              }
              // Owner's admin user-detail page (device list + per-device quota)
              // reflects device add/edit/delete/quota changes in real time.
              if (event.user_id) {
                void qc.invalidateQueries({
                  queryKey: ["admin", "user", event.user_id],
                })
              }
              // OS heads-up for the user's *other* (backgrounded) sessions.
              // osNotify only fires while the tab is hidden, so the session
              // that performed the action (already showed its own toast) and
              // any actively-viewing session aren't double-notified.
              if (label) {
                osNotify(`Device ${label}`, {
                  body: name,
                  url: event.id ? `/app/devices/${event.id}` : "/app/devices",
                  tag: event.id ? `dev-${event.id}-${event.action}` : undefined,
                })
              }
              break
            }
            case "user":
              void qc.invalidateQueries({ queryKey: ["admin", "users"] })
              void qc.invalidateQueries({ queryKey: ["admin", "stats"] })
              void qc.invalidateQueries({ queryKey: ["me", "topology"] })
              if (event.id) {
                void qc.invalidateQueries({
                  queryKey: ["admin", "user", event.id],
                })
              }
              break
            case "server":
              void qc.invalidateQueries({ queryKey: ["admin", "servers"] })
              void qc.invalidateQueries({ queryKey: ["me", "server"] })
              if (event.id) {
                void qc.invalidateQueries({
                  queryKey: ["admin", "server", event.id],
                })
              }
              break
            case "maintenance":
              void qc.invalidateQueries({ queryKey: ["admin", "maintenance"] })
              void qc.invalidateQueries({ queryKey: ["maintenance-banner"] })
              break
          }
          break
        }
        case "notify": {
          // Server-composed notification (connectivity / quota / security).
          // Render it as a toast + OS notification (the latter only when the
          // tab is hidden, handled inside notify()). Processed regardless of
          // visibility so the OS heads-up still fires for a backgrounded tab.
          const fn = notify[event.level]
          fn(event.title, {
            description: event.body ?? undefined,
            important: true,
            url: event.url ?? undefined,
            id: event.tag ?? undefined,
          })
          break
        }
        default:
          // heartbeat — tail handled above
          break
      }
    },
    [applyDelta, applyServerHealth, applyServerSample, pushTail, qc],
  )

  // When the tab regains focus after we skipped any deltas, refetch the
  // devices list so live status / handshake / quotas are current. The
  // per-device charts will re-hydrate from their history endpoints via
  // the dashboard's useHistoryHydration call.
  useEffect(() => {
    const onVis = () => {
      if (isPageVisible() && skippedWhileHiddenRef.current) {
        skippedWhileHiddenRef.current = false
        void qc.invalidateQueries({ queryKey: ["devices"] })
      }
    }
    document.addEventListener("visibilitychange", onVis)
    return () => document.removeEventListener("visibilitychange", onVis)
  }, [qc])

  useWebSocket({
    path: "/api/v1/ws",
    onEvent,
    enabled: !!user,
  })

  return null
}
