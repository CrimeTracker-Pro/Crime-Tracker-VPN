import { useEffect, useMemo, useState } from "react"

import { ACTIVITY_STALE_MS } from "@/hooks/useDeviceOnline"
import { useNow } from "@/hooks/useNow"
import type { OnlineDeviceRow } from "@/lib/api"
import { currentOnlineDevices } from "@/lib/onlineDevices"
import { useLiveStats } from "@/stores/liveStats"

/** Same handshake + keepalive rule for the overview count and the roster. */
export function useOnlineDeviceRows(
  rows: OnlineDeviceRow[] | undefined,
): OnlineDeviceRow[] {
  const now = useNow()
  const live = useLiveStats((s) => s.devices)
  const [visibleSince, setVisibleSince] = useState(() => Date.now())
  useEffect(() => {
    const onVisibilityChange = () => {
      if (document.visibilityState !== "hidden") setVisibleSince(Date.now())
    }
    document.addEventListener("visibilitychange", onVisibilityChange)
    return () => document.removeEventListener("visibilitychange", onVisibilityChange)
  }, [])
  const seen = useMemo(() => {
    const values: Record<string, number> = {}
    for (const [id, device] of Object.entries(live)) values[id] = device.lastSeenTs
    return values
  }, [live])
  return useMemo(
    () =>
      currentOnlineDevices(
        rows ?? [],
        seen,
        now,
        now - visibleSince > ACTIVITY_STALE_MS,
      ),
    [rows, seen, now, visibleSince],
  )
}
