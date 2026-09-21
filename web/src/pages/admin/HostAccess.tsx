import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { IconLockAccess, IconPlus, IconTrash } from "@tabler/icons-react"
import { useEffect, useState } from "react"
import { toast } from "sonner"

import { PageStagger, StaggerItem } from "@/components/motion"
import { PageHead, Panel } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import {
  adminCreateHostAccess, adminDeleteHostAccess, adminListHostAccess,
  adminListHostAccessDevices, adminUpdateHostAccess,
} from "@/lib/api"
import type { HostAccessRule, SaveHostAccessRule } from "@/lib/api"

const blank: SaveHostAccessRule = {
  name: "", description: "", protocol: "tcp", gateway_port: 0,
  backend_port: 0, enabled: true, allow_all_peers: false, device_ids: [],
}

export function HostAccessPage() {
  const qc = useQueryClient()
  const rules = useQuery({ queryKey: ["admin", "host-access"], queryFn: adminListHostAccess })
  const devices = useQuery({ queryKey: ["admin", "host-access", "devices"], queryFn: adminListHostAccessDevices })
  const [creating, setCreating] = useState(false)
  const refresh = () => qc.invalidateQueries({ queryKey: ["admin", "host-access"] })

  return <PageStagger>
    <StaggerItem>
      <PageHead eyebrow="Admin · Security" title="Host access" sub="default deny · immediate firewall apply" />
    </StaggerItem>
    <StaggerItem className="flex justify-end">
      <Button onClick={() => setCreating(true)} disabled={creating}><IconPlus />Add rule</Button>
    </StaggerItem>
    {creating && <StaggerItem><RuleEditor devices={devices.data ?? []} onCancel={() => setCreating(false)} onSaved={() => { setCreating(false); refresh() }} /></StaggerItem>}
    {rules.data?.map(rule => <StaggerItem key={rule.id}><RuleEditor rule={rule} devices={devices.data ?? []} onSaved={refresh} /></StaggerItem>)}
    {rules.data?.length === 0 && <StaggerItem><Panel title="No host services"><p className="text-sm text-muted-foreground">VPN peers cannot reach host services until a rule is created.</p></Panel></StaggerItem>}
  </PageStagger>
}

function RuleEditor({ rule, devices, onSaved, onCancel }: {
  rule?: HostAccessRule
  devices: Awaited<ReturnType<typeof adminListHostAccessDevices>>
  onSaved: () => void
  onCancel?: () => void
}) {
  const [form, setForm] = useState<SaveHostAccessRule>(rule ? { ...rule } : blank)
  useEffect(() => { if (rule) setForm({ ...rule }) }, [rule])
  const save = useMutation({
    mutationFn: () => rule ? adminUpdateHostAccess(rule.id, form) : adminCreateHostAccess(form),
    onSuccess: () => { toast.success(rule ? "Host access updated" : "Host access created"); onSaved() },
    onError: (e: Error) => toast.error(e.message),
  })
  const remove = useMutation({
    mutationFn: () => adminDeleteHostAccess(rule!.id),
    onSuccess: () => { toast.success("Host access removed"); onSaved() },
    onError: (e: Error) => toast.error(e.message),
  })
  const set = <K extends keyof SaveHostAccessRule>(key: K, value: SaveHostAccessRule[K]) => setForm(v => ({ ...v, [key]: value }))
  const toggleDevice = (id: string, checked: boolean) => set("device_ids", checked ? [...form.device_ids, id] : form.device_ids.filter(x => x !== id))

  return <Panel title={<span className="inline-flex items-center gap-2"><IconLockAccess className="size-4" />{rule?.name || "New host access rule"}</span>} sub={`${rule?.gateway_ip ?? "10.0.0.1"}:${form.gateway_port || "port"} → ${rule?.backend_ip ?? "192.168.1.20"}:${form.backend_port || "port"}`}>
    <div className="grid gap-3 sm:grid-cols-2">
      <Field label="Rule name"><Input value={form.name} onChange={e => set("name", e.target.value)} placeholder="Example: PostgreSQL" /></Field>
      <Field label="Protocol"><Select value={form.protocol} onValueChange={v => set("protocol", v as "tcp" | "udp")}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="tcp">TCP</SelectItem><SelectItem value="udp">UDP</SelectItem></SelectContent></Select></Field>
      <Field label="VPN gateway port"><Input type="number" min={1} max={65535} value={form.gateway_port || ""} onChange={e => set("gateway_port", Number(e.target.value))} /></Field>
      <Field label="Host service port"><Input type="number" min={1} max={65535} value={form.backend_port || ""} onChange={e => set("backend_port", Number(e.target.value))} /></Field>
    </div>
    <Field label="Description"><Input value={form.description} onChange={e => set("description", e.target.value)} /></Field>
    <div className="flex items-center justify-between border p-3"><div><p className="text-sm font-medium">Enabled</p><p className="text-xs text-muted-foreground">Apply this permission to the live firewall.</p></div><Switch checked={form.enabled} onCheckedChange={v => set("enabled", v)} /></div>
    <div className="flex items-center justify-between border p-3"><div><p className="text-sm font-medium">All active VPN peers</p><p className="text-xs text-muted-foreground">New peers automatically inherit this service.</p></div><Switch checked={form.allow_all_peers} onCheckedChange={v => set("allow_all_peers", v)} /></div>
    {!form.allow_all_peers && <div className="space-y-2"><Label className="zv-eyebrow">Allowed devices</Label>{devices.map(d => <label key={d.id} className="flex cursor-pointer items-center gap-3 border p-3"><Checkbox checked={form.device_ids.includes(d.id)} onCheckedChange={v => toggleDevice(d.id, v === true)} /><span className="min-w-0"><span className="block text-sm font-medium">{d.name} · {d.allocated_ip}</span><span className="block truncate text-xs text-muted-foreground">{d.owner_email} · {d.status}</span></span></label>)}</div>}
    <div className="flex justify-between gap-2 pt-2"><div>{rule && <Button variant="destructive" onClick={() => { if (confirm(`Delete ${rule.name}? Access stops immediately.`)) remove.mutate() }} disabled={remove.isPending}><IconTrash />Delete</Button>}</div><div className="flex gap-2">{onCancel && <Button variant="outline" onClick={onCancel}>Cancel</Button>}<Button onClick={() => save.mutate()} disabled={save.isPending}>{save.isPending ? "Applying…" : "Save and apply"}</Button></div></div>
  </Panel>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="flex flex-col gap-1.5"><Label className="zv-eyebrow">{label}</Label>{children}</div>
}
