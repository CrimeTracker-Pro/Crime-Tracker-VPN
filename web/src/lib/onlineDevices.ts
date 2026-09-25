import type { OnlineDeviceRow } from "@/lib/api"
import { ACTIVITY_STALE_MS } from "@/hooks/useDeviceOnline"

const ONLINE_HANDSHAKE_WINDOW_MS = 180_000

/** Matches the admin API and overview KPI's active + recent-handshake rule. */
export function currentOnlineDevices(
  rows: OnlineDeviceRow[],
  lastSeenByDevice: Record<string, number>,
  now: number,
  settled: boolean,
): OnlineDeviceRow[] {
  return rows.filter((row) => {
    const handshake = Date.parse(row.last_handshake_at)
    if (
      !Number.isFinite(handshake) ||
      now - handshake >= ONLINE_HANDSHAKE_WINDOW_MS
    ) return false
    const lastSeen = lastSeenByDevice[row.id] ?? 0
    return !(
      settled &&
      lastSeen > 0 &&
      now - Math.max(handshake, lastSeen) > ACTIVITY_STALE_MS
    )
  })
}
