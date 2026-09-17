import { useQuery } from "@tanstack/react-query"
import {
  IconDeviceDesktop,
  IconRouter,
  IconUsers,
} from "@tabler/icons-react"
import { Link, useParams } from "react-router"

import { CandleChart } from "@/components/charts/CandleChart"
import { CopyableCode } from "@/components/CopyableCode"
import { Identicon } from "@/components/Identicon"
import { PageStagger, StaggerItem } from "@/components/motion"
import { RelativeTime } from "@/components/RelativeTime"
import { PageHead, Panel, Pill } from "@/components/swiss"
import { StatusPill, type Status } from "@/components/StatusPill"
import { Skeleton } from "@/components/ui/skeleton"
import {
  ApiError,
  type DeviceStatus,
  adminGetServerDetail,
} from "@/lib/api"

const DEVICE_STATUS_TO_PILL: Record<DeviceStatus, Status> = {
  active: "active",
  paused: "paused",
  revoked: "revoked",
}

export function ServerDetailPage() {
  const { id = "" } = useParams<{ id: string }>()

  const detailQ = useQuery({
    queryKey: ["admin", "server", id],
    queryFn: () => adminGetServerDetail(id),
    enabled: !!id,
  })

  const s = detailQ.data?.server

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow={`Admin · Server · ${id.slice(0, 8)}`}
          title={
            s ? (
              <span className="inline-flex items-center gap-3">
                <span className="border-border bg-card flex size-9 shrink-0 items-center justify-center border">
                  <IconRouter className="size-5" />
                </span>
                <span className="truncate">{s.name}</span>
              </span>
            ) : detailQ.isLoading ? (
              "Loading…"
            ) : (
              "Unknown server"
            )
          }
          sub={
            s && (
              <span className="flex flex-wrap items-center gap-2">
                <Pill tone={s.is_active ? "ok" : "warn"} dot={false}>
                  {s.is_active ? "active" : "disabled"}
                </Pill>
                <span className="text-muted-foreground font-mono text-[11px]">
                  {s.region} · {s.endpoint_host}:{s.endpoint_port} · {s.cidr}
                </span>
              </span>
            )
          }
        />
      </StaggerItem>

      {detailQ.isLoading && (
        <StaggerItem>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            <Skeleton className="h-40 rounded-none" />
            <Skeleton className="h-40 rounded-none" />
          </div>
        </StaggerItem>
      )}

      {s && detailQ.data && (
        <>
          <StaggerItem>
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
              <Panel title="Configuration">
                <KvList
                  items={[
                    [
                      "ID",
                      <code key="id" className="font-mono text-[11px]">
                        {s.id}
                      </code>,
                    ],
                    ["Region", s.region],
                    [
                      "Endpoint",
                      <code key="ep" className="font-mono text-xs">
                        {s.endpoint_host}:{s.endpoint_port}
                      </code>,
                    ],
                    [
                      "CIDR",
                      <code key="cidr" className="font-mono text-xs">
                        {s.cidr}
                      </code>,
                    ],
                    ["MTU", String(s.mtu)],
                  ]}
                />
                <div className="mt-4 flex flex-col gap-1.5">
                  <span className="text-muted-foreground font-mono text-[10px] uppercase tracking-wide">
                    Public key
                  </span>
                  <CopyableCode value={s.public_key} truncate />
                </div>
              </Panel>

              <Panel title="Devices on this server">
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                  <Stat
                    label="Active"
                    value={detailQ.data.device_count_active}
                    icon={<IconUsers className="size-3.5" />}
                  />
                  <Stat
                    label="Paused"
                    value={detailQ.data.device_count_paused}
                  />
                  <Stat
                    label="Total"
                    value={detailQ.data.device_count_total}
                  />
                </div>
              </Panel>
            </div>
          </StaggerItem>

          <StaggerItem>
            <CandleChart
              scope="server"
              id={id}
              height={260}
              title="Bandwidth"
              sub="Aggregated across every device on this server · scroll to zoom · drag to pan"
            />
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Devices"
              sub={`${detailQ.data.devices.length} non-revoked device${detailQ.data.devices.length === 1 ? "" : "s"} (top 200 by created_at)`}
              flush
            >
              {detailQ.data.devices.length > 0 ? (
                <div className="zv-table-scroll">
                  <table className="zv-table">
                    <thead>
                      <tr>
                        <th>Owner</th>
                        <th>Device</th>
                        <th>OS</th>
                        <th>IP</th>
                        <th>Status</th>
                        <th>Last handshake</th>
                        <th>Created</th>
                      </tr>
                    </thead>
                    <tbody>
                      {detailQ.data.devices.map((d) => (
                        <tr key={d.id}>
                          <td>
                            <Link
                              to={`/admin/users/${d.user_id}`}
                              className="hover:text-primary inline-flex items-center gap-2 transition-colors"
                            >
                              <span className="border-border bg-card flex size-5 shrink-0 items-center justify-center border p-0.5">
                                <Identicon seed={d.user_email} size={16} cells={5} />
                              </span>
                              <span className="font-medium underline-offset-2 hover:underline">
                                {d.user_email}
                              </span>
                            </Link>
                          </td>
                          <td>
                            <span className="inline-flex items-center gap-2">
                              <IconDeviceDesktop className="text-muted-foreground size-4" />
                              <span className="font-medium">{d.name}</span>
                            </span>
                          </td>
                          <td className="text-muted-foreground capitalize">
                            {d.os}
                          </td>
                          <td className="font-mono text-xs">{d.allocated_ip}</td>
                          <td>
                            <StatusPill
                              status={DEVICE_STATUS_TO_PILL[d.status] ?? "offline"}
                              label={d.status}
                            />
                          </td>
                          <td className="text-muted-foreground font-mono text-xs">
                            <RelativeTime
                              value={d.last_handshake_at}
                              fallback="Never"
                            />
                          </td>
                          <td className="text-muted-foreground font-mono text-xs">
                            <RelativeTime value={d.created_at} fallback="—" />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="text-muted-foreground py-8 text-center font-mono text-sm">
                  No devices on this server.
                </div>
              )}
            </Panel>
          </StaggerItem>
        </>
      )}

      {detailQ.isError && (
        <StaggerItem>
          <Panel title="Failed to load">
            <p className="text-muted-foreground text-sm">
              {detailQ.error instanceof ApiError
                ? detailQ.error.message
                : "Could not fetch server."}
            </p>
          </Panel>
        </StaggerItem>
      )}
    </PageStagger>
  )
}

function KvList({ items }: { items: [string, React.ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
      {items.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-muted-foreground font-mono text-xs uppercase tracking-wide">
            {k}
          </dt>
          <dd className="text-foreground min-w-0 truncate">{v}</dd>
        </div>
      ))}
    </dl>
  )
}

function Stat({
  label,
  value,
  icon,
}: {
  label: string
  value: number
  icon?: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-muted-foreground inline-flex items-center gap-1.5 font-mono text-[10px] uppercase tracking-wide">
        {icon}
        {label}
      </span>
      <span className="text-foreground font-mono text-2xl tabular-nums">
        {value}
      </span>
    </div>
  )
}
