import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { IconExternalLink, IconKey, IconRouter } from "@tabler/icons-react"
import { useState } from "react"
import { Link } from "react-router"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { CopyableCode } from "@/components/CopyableCode"
import { EmptyState } from "@/components/EmptyState"
import { PageStagger, StaggerItem } from "@/components/motion"
import { PageHead, Panel } from "@/components/swiss"
import { StatusPill } from "@/components/StatusPill"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Skeleton } from "@/components/ui/skeleton"
import {
  adminListServers,
  adminPatchServer,
  adminRotateServerKeys,
} from "@/lib/api"
import type { AdminServerRow } from "@/lib/api"

export function ServersPage() {
  const q = useQuery({
    queryKey: ["admin", "servers"],
    queryFn: adminListServers,
  })

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Admin · 06"
          title="Servers"
          sub="hubs · keypairs · rotation · drain"
        />
      </StaggerItem>
      {q.isLoading && (
        <StaggerItem className="flex flex-col gap-3">
          <Skeleton className="h-48 rounded-none" />
          <Skeleton className="h-48 rounded-none" />
        </StaggerItem>
      )}
      {q.data && q.data.length === 0 && (
        <StaggerItem>
          <EmptyState
            icon={IconRouter}
            title="No servers configured"
            description="Bootstrap creates a default server on first boot."
          />
        </StaggerItem>
      )}
      {q.data?.map((s) => (
        <StaggerItem key={s.id}>
          <ServerEditor server={s} />
        </StaggerItem>
      ))}
    </PageStagger>
  )
}

function ServerEditor({ server }: { server: AdminServerRow }) {
  const qc = useQueryClient()
  const [endpointHost, setEndpointHost] = useState(server.endpoint_host)
  const [endpointPort, setEndpointPort] = useState(String(server.endpoint_port))
  const [mtu, setMtu] = useState(String(server.mtu))
  const [keepalive, setKeepalive] = useState(String(server.persistent_keepalive))
  const [defaultAllowedIps, setDefaultAllowedIps] = useState(server.default_allowed_ips.join(", "))
  const [rotateOpen, setRotateOpen] = useState(false)

  const save = useMutation({
    mutationFn: () => {
      // Treat empty / NaN keepalive as "leave unchanged"; 0 is valid (disables).
      const ka = keepalive.trim() === "" ? NaN : Number(keepalive)
      return adminPatchServer(server.id, {
        endpoint_host: endpointHost.trim() || undefined,
        endpoint_port: Number(endpointPort) || undefined,
        mtu: Number(mtu) || undefined,
        persistent_keepalive: Number.isFinite(ka) ? ka : undefined,
        default_allowed_ips: defaultAllowedIps.split(/[\n,]/).map((route) => route.trim()).filter(Boolean),
      })
    },
    onSuccess: () => {
      toast.success("Server config saved")
      qc.invalidateQueries({ queryKey: ["admin", "servers"] })
    },
    onError: (e: Error) => toast.error(e.message),
  })

  const rotate = useMutation({
    mutationFn: () => adminRotateServerKeys(server.id),
    onSuccess: (data) => {
      toast.success("Server keys rotated", { description: data.warning })
      setRotateOpen(false)
      qc.invalidateQueries({ queryKey: ["admin", "servers"] })
    },
    onError: (e: Error) => toast.error(e.message),
  })

  return (
    <Panel
      title={
        <span className="inline-flex items-center gap-2">
          <IconRouter className="size-4" />
          {server.name}
        </span>
      }
      sub={`${server.region} · ${server.cidr}`}
      right={
        <div className="flex items-center gap-2">
          <Button asChild size="sm" variant="ghost">
            <Link to={`/admin/servers/${server.id}`}>
              <IconExternalLink className="size-3.5" />
              Detail
            </Link>
          </Button>
          <StatusPill status={server.is_active ? "active" : "offline"} />
        </div>
      }
      footer={
        <div className="flex w-full justify-between">
          <Button onClick={() => save.mutate()} disabled={save.isPending}>
            {save.isPending ? "Saving…" : "Save"}
          </Button>
          <Button
            variant="destructive"
            onClick={() => setRotateOpen(true)}
            disabled={rotate.isPending}
          >
            <IconKey />
            {rotate.isPending ? "Rotating…" : "Rotate keys"}
          </Button>
        </div>
      }
    >
      <div className="flex flex-col gap-1.5">
        <Label className="zv-eyebrow">Public key</Label>
        <CopyableCode value={server.public_key} />
      </div>
      <div className="mt-3 flex flex-col gap-1.5">
        <Label htmlFor={`allowed-ips-${server.id}`} className="zv-eyebrow">
          Default AllowedIPs for new peers
        </Label>
        <Input
          id={`allowed-ips-${server.id}`}
          value={defaultAllowedIps}
          onChange={(e) => setDefaultAllowedIps(e.target.value)}
          placeholder="192.168.1.20/32, 192.168.1.0/24"
          className="font-mono"
        />
        <p className="text-xs text-muted-foreground">
          Comma-separated routes. The VPN subnet ({server.cidr}) is always included.
        </p>
      </div>
      <div className="mt-4 grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={`eh-${server.id}`} className="zv-eyebrow">
            Endpoint host
          </Label>
          <Input
            id={`eh-${server.id}`}
            value={endpointHost}
            onChange={(e) => setEndpointHost(e.target.value)}
            className="font-mono"
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={`ep-${server.id}`} className="zv-eyebrow">
            Endpoint port
          </Label>
          <Input
            id={`ep-${server.id}`}
            type="number"
            value={endpointPort}
            onChange={(e) => setEndpointPort(e.target.value)}
            className="font-mono"
          />
        </div>
      </div>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={`mtu-${server.id}`} className="zv-eyebrow">
            MTU
          </Label>
          <Input
            id={`mtu-${server.id}`}
            type="number"
            value={mtu}
            onChange={(e) => setMtu(e.target.value)}
            className="font-mono"
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={`ka-${server.id}`} className="zv-eyebrow">
            Persistent keepalive (seconds)
          </Label>
          <Input
            id={`ka-${server.id}`}
            type="number"
            min={0}
            max={3600}
            value={keepalive}
            onChange={(e) => setKeepalive(e.target.value)}
            placeholder="30 (0 disables)"
            className="font-mono"
          />
        </div>
      </div>
      <ConfirmDialog
        open={rotateOpen}
        onOpenChange={setRotateOpen}
        title="Rotate server keys?"
        description={`Generates a new keypair, rewrites wg0.conf on the shared volume, and persists the new pubkey. All peer .conf files must be re-downloaded; the wg container needs a restart.`}
        confirmText={server.name}
        confirmLabel="Rotate keys"
        destructive
        pending={rotate.isPending}
        onConfirm={() => rotate.mutate()}
      />
    </Panel>
  )
}
