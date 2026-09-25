import { useQuery } from "@tanstack/react-query"
import { useMemo } from "react"

import { LiveIndicator } from "@/components/charts/LiveIndicator"
import {
  FinderDeviceCard,
  OwnerAccordion,
} from "@/components/finder/FinderResults"
import { PageStagger, StaggerItem } from "@/components/motion"
import { PageHead, Panel } from "@/components/swiss"
import { Skeleton } from "@/components/ui/skeleton"
import { useOnlineDeviceRows } from "@/hooks/useOnlineDeviceRows"
import { adminOnlineDevices, type OnlineDeviceRow } from "@/lib/api"

export function AdminOnlineDevicesPage() {
  const devicesQ = useQuery({
    queryKey: ["admin", "online-devices"],
    queryFn: adminOnlineDevices,
    refetchInterval: 5_000,
  })
  const online = useOnlineDeviceRows(devicesQ.data)
  const groups = useMemo(() => {
    const byOwner = new Map<
      string,
      { email: string; devices: OnlineDeviceRow[] }
    >()
    for (const device of online) {
      const group = byOwner.get(device.user_id)
      if (group) group.devices.push(device)
      else {
        byOwner.set(device.user_id, {
          email: device.user_email,
          devices: [device],
        })
      }
    }
    return byOwner
  }, [online])

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Admin · Devices"
          title="Online now"
          sub="Active WireGuard peers with a handshake in the last 3 minutes"
        />
      </StaggerItem>
      <StaggerItem>
        <Panel
          title={`${online.length} online ${online.length === 1 ? "device" : "devices"}`}
          sub="Roster refreshes every 5 seconds · traffic cards update over the live stream"
          right={<LiveIndicator />}
        >
          {devicesQ.isError && (
            <p className="mb-3 font-mono text-sm text-destructive">
              Could not refresh the roster; shown entries may be stale. Retrying automatically.
            </p>
          )}
          {devicesQ.isPending ? (
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-3">
              <Skeleton className="h-32 rounded-none" />
              <Skeleton className="h-32 rounded-none" />
              <Skeleton className="h-32 rounded-none" />
            </div>
          ) : devicesQ.isError && !devicesQ.data ? null : online.length === 0 ? (
            <p className="font-mono text-sm text-muted-foreground">
              No devices are online right now.
            </p>
          ) : (
            <div className="flex flex-col gap-3">
              {[...groups.entries()].map(([userId, group]) => (
                <OwnerAccordion
                  key={userId}
                  email={group.email}
                  count={group.devices.length}
                  to={`/admin/users/${userId}`}
                >
                  {group.devices.map((device) => (
                    <FinderDeviceCard
                      key={device.id}
                      deviceId={device.id}
                      name={device.name}
                      ip={device.allocated_ip}
                      to={`/admin/devices/${device.id}`}
                      online
                    />
                  ))}
                </OwnerAccordion>
              ))}
            </div>
          )}
        </Panel>
      </StaggerItem>
    </PageStagger>
  )
}
