import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { IconLockAccess } from "@tabler/icons-react"
import { useState } from "react"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { PageStagger, StaggerItem } from "@/components/motion"
import { PageHead, Panel } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  adminApplyPolicy,
  adminCreatePolicyService,
  adminDeletePolicyService,
  adminGetPolicyAssignments,
  adminListPolicyServices,
  adminListServers,
  adminPolicyRevisions,
  adminPolicyStatus,
  adminSetPolicyMode,
  adminValidatePolicy,
} from "@/lib/api"

export function AccessPolicyPage() {
  const qc = useQueryClient()
  const services = useQuery({ queryKey: ["admin", "policy", "services"], queryFn: adminListPolicyServices })
  const status = useQuery({ queryKey: ["admin", "policy", "status"], queryFn: adminPolicyStatus, refetchInterval: 5000 })
  const revisions = useQuery({ queryKey: ["admin", "policy", "revisions"], queryFn: adminPolicyRevisions })
  const servers = useQuery({ queryKey: ["admin", "servers"], queryFn: adminListServers })
  const [name, setName] = useState("")
  const [protocol, setProtocol] = useState<"tcp" | "udp">("tcp")
  const [gatewayPort, setGatewayPort] = useState("")
  const [backendIp, setBackendIp] = useState("192.168.1.20")
  const [backendPort, setBackendPort] = useState("")
  const [confirmEnforce, setConfirmEnforce] = useState(false)

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["admin", "policy"] })
  }
  const create = useMutation({
    mutationFn: () => adminCreatePolicyService({
      server_id: servers.data![0].id,
      name: name.trim(), description: "", protocol,
      gateway_ip: "10.0.0.1", gateway_port: Number(gatewayPort),
      backend_ip: backendIp.trim(), backend_port: Number(backendPort), enabled: true,
    }),
    onSuccess: () => { toast.success("Service mapping created"); setName(""); refresh() },
    onError: (e: Error) => toast.error(e.message),
  })
  const validate = useMutation({
    mutationFn: adminValidatePolicy,
    onSuccess: (v) => v.valid ? toast.success("Policy is valid", { description: v.warnings.join(" · ") || undefined }) : toast.error(v.errors.join(" · ")),
    onError: (e: Error) => toast.error(e.message),
  })
  const apply = useMutation({ mutationFn: adminApplyPolicy, onSuccess: () => { toast.success("Policy reconcile queued"); refresh() }, onError: (e: Error) => toast.error(e.message) })
  const enforce = useMutation({ mutationFn: () => adminSetPolicyMode("enforce"), onSuccess: () => { setConfirmEnforce(false); toast.success("Enforcement queued"); refresh() }, onError: (e: Error) => toast.error(e.message) })
  const shadow = useMutation({ mutationFn: () => adminSetPolicyMode("shadow"), onSuccess: () => { toast.success("Shadow mode queued"); refresh() }, onError: (e: Error) => toast.error(e.message) })

  const runtime = status.data?.[0]
  return <PageStagger>
    <StaggerItem><PageHead eyebrow="Admin · Security" title="VPN access policy" sub="service gateway · assignments · atomic firewall policy" /></StaggerItem>
    <StaggerItem>
      <Panel title="Runtime" sub={runtime ? `${runtime.reconciler_mode} · generation ${runtime.applied_generation}/${runtime.desired_generation}` : "Loading"}>
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" onClick={() => validate.mutate()}>Validate</Button>
          <Button variant="outline" onClick={() => apply.mutate()}>Compile & apply</Button>
          {runtime?.reconciler_mode === "enforce"
            ? <Button variant="destructive" onClick={() => shadow.mutate()}>Return to shadow</Button>
            : <Button variant="destructive" onClick={() => setConfirmEnforce(true)}>Enable default-deny</Button>}
        </div>
        {runtime?.last_error && <p className="mt-3 text-sm text-destructive">{runtime.last_error}</p>}
      </Panel>
    </StaggerItem>
    <StaggerItem>
      <Panel title="New service mapping" sub="10.0.0.1 → approved backend">
        <div className="grid gap-3 md:grid-cols-5">
          <div><Label>Name</Label><Input value={name} onChange={(e) => setName(e.target.value)} placeholder="SMB" /></div>
          <div><Label>Protocol</Label><select className="h-9 w-full border bg-background px-2" value={protocol} onChange={(e) => setProtocol(e.target.value as "tcp" | "udp")}><option value="tcp">TCP</option><option value="udp">UDP</option></select></div>
          <div><Label>Gateway port</Label><Input type="number" value={gatewayPort} onChange={(e) => setGatewayPort(e.target.value)} placeholder="445" /></div>
          <div><Label>Backend IP</Label><Input value={backendIp} onChange={(e) => setBackendIp(e.target.value)} /></div>
          <div><Label>Backend port</Label><Input type="number" value={backendPort} onChange={(e) => setBackendPort(e.target.value)} placeholder="445" /></div>
        </div>
        <Button className="mt-3" disabled={!servers.data?.[0] || !name || !gatewayPort || !backendPort || create.isPending} onClick={() => create.mutate()}>Create mapping</Button>
      </Panel>
    </StaggerItem>
    <StaggerItem>
      <Panel title="Services" sub={`${services.data?.length ?? 0} configured`}>
        <div className="divide-y border">
          {services.data?.map((s) => <ServiceRow key={s.id} service={s} refresh={refresh} />)}
          {!services.data?.length && <div className="p-6 text-center text-sm text-muted-foreground"><IconLockAccess className="mx-auto mb-2" />No service mappings yet.</div>}
        </div>
      </Panel>
    </StaggerItem>
    <StaggerItem><Panel title="Revision history" sub="validated and applied policy snapshots"><div className="space-y-2 font-mono text-xs">{revisions.data?.slice(0, 10).map((r) => <div key={r.id} className="flex justify-between border-b pb-2"><span>#{r.revision_number} · {r.status}</span><span>{r.checksum.slice(0, 12)}</span></div>)}</div></Panel></StaggerItem>
    <ConfirmDialog open={confirmEnforce} onOpenChange={setConfirmEnforce} title="Enable default-deny enforcement?" description="This atomically replaces the application-owned VPN firewall tables. Unassigned services and all other wg0 forwarding will be denied." confirmLabel="Enable enforcement" destructive onConfirm={() => enforce.mutate()} />
  </PageStagger>
}

function ServiceRow({ service: s, refresh }: { service: Awaited<ReturnType<typeof adminListPolicyServices>>[number]; refresh: () => void }) {
  const assignments = useQuery({ queryKey: ["admin", "policy", "assignments", s.id], queryFn: () => adminGetPolicyAssignments(s.id) })
  return <div className="flex items-start justify-between gap-3 p-3">
    <div className="min-w-0">
      <p className="font-medium">{s.name}</p>
      <p className="font-mono text-xs text-muted-foreground">{s.protocol.toUpperCase()} {s.gateway_ip}:{s.gateway_port} → {s.backend_ip}:{s.backend_port}</p>
      {assignments.data && <div className="mt-2 text-xs text-muted-foreground">
        <p><span className="font-medium text-foreground">Assigned users:</span> {assignments.data.users.length ? assignments.data.users.map((u) => `${u.email}${u.role === "admin" ? " (admin)" : ""}`).join(", ") : "None"}</p>
        <p className="mt-1"><span className="font-medium text-foreground">Effective devices ({assignments.data.devices.length}):</span> {assignments.data.devices.length ? assignments.data.devices.map((d) => `${d.name} (${d.allocated_ip})`).join(", ") : "None"}</p>
      </div>}
    </div>
    <Button variant="destructive" size="sm" onClick={() => adminDeletePolicyService(s.id).then(refresh).catch((e: Error) => toast.error(e.message))}>Delete</Button>
  </div>
}
