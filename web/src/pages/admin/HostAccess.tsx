import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { IconCheck, IconChevronDown, IconDeviceLaptop, IconEdit, IconLockAccess, IconPlus, IconTrash, IconX } from "@tabler/icons-react"
import { useEffect, useMemo, useState } from "react"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { EmptyState } from "@/components/EmptyState"
import { PageStagger, StaggerItem } from "@/components/motion"
import { PageHead, Panel } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { Switch } from "@/components/ui/switch"
import { adminCreateHostAccess, adminDeleteHostAccess, adminListHostAccess, adminListHostAccessDevices, adminUpdateHostAccess } from "@/lib/api"
import type { HostAccessDevice, HostAccessRule, SaveHostAccessRule } from "@/lib/api"
import { cn } from "@/lib/utils"

const newRule = (): SaveHostAccessRule => ({
  name: "", description: "", protocol: "tcp", gateway_port: 0,
  backend_port: 0, enabled: true, allow_all_peers: false, device_ids: [],
})

export function HostAccessPage() {
  const qc = useQueryClient()
  const rules = useQuery({ queryKey: ["admin", "host-access"], queryFn: adminListHostAccess })
  const devices = useQuery({ queryKey: ["admin", "host-access", "devices"], queryFn: adminListHostAccessDevices })
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<HostAccessRule>()
  const [deleting, setDeleting] = useState<HostAccessRule>()
  const refresh = () => qc.invalidateQueries({ queryKey: ["admin", "host-access"] })
  const remove = useMutation({
    mutationFn: (id: string) => adminDeleteHostAccess(id),
    onSuccess: () => { toast.success("Host access rule deleted"); setDeleting(undefined); refresh() },
    onError: (error: Error) => toast.error(error.message),
  })
  const openCreate = () => { setEditing(undefined); setEditorOpen(true) }
  const openEdit = (rule: HostAccessRule) => { setEditing(rule); setEditorOpen(true) }

  return <PageStagger>
    <StaggerItem><PageHead eyebrow="Admin · Security" title="Host access" sub="default deny · immediate firewall apply" /></StaggerItem>
    <StaggerItem>
      <Panel title="Access rules" sub="VPN gateway → host services" right={<Button onClick={openCreate}><IconPlus />New rule</Button>}>
        <p className="mb-4 text-xs text-muted-foreground">
          The host destination is managed automatically as <span className="font-mono text-foreground">192.168.1.20</span>. Firewall output shows <span className="font-mono text-foreground">/32</span> because access is restricted to this single host.
        </p>
        {(rules.isLoading || devices.isLoading) && <Skeleton className="h-44 rounded-none" />}
        {rules.data?.length === 0 && <EmptyState icon={IconLockAccess} title="No host access rules" description="VPN peers cannot reach host services until a rule is created." action={<Button onClick={openCreate}><IconPlus />New rule</Button>} />}
        {rules.data && rules.data.length > 0 && <div className="overflow-x-auto border">
          <table className="zv-table min-w-[820px]">
            <thead><tr><th>Rule</th><th>Flow</th><th>Protocol</th><th>Devices</th><th>Status</th><th className="text-right">Actions</th></tr></thead>
            <tbody>{rules.data.map((rule) => <RuleRow key={rule.id} rule={rule} devices={devices.data ?? []} onEdit={() => openEdit(rule)} onDelete={() => setDeleting(rule)} />)}</tbody>
          </table>
        </div>}
      </Panel>
    </StaggerItem>
    <RuleSheet key={editing?.id ?? "new"} open={editorOpen} onOpenChange={setEditorOpen} rule={editing} devices={devices.data ?? []} onSaved={() => { setEditorOpen(false); refresh() }} />
    <ConfirmDialog open={Boolean(deleting)} onOpenChange={(open) => { if (!open) setDeleting(undefined) }} title={`Delete ${deleting?.name ?? "rule"}?`} description="The selected VPN peers will lose this host service immediately. This action cannot be undone." confirmLabel="Delete rule" destructive pending={remove.isPending} onConfirm={() => deleting && remove.mutate(deleting.id)} />
  </PageStagger>
}

function RuleRow({ rule, devices, onEdit, onDelete }: { rule: HostAccessRule; devices: HostAccessDevice[]; onEdit: () => void; onDelete: () => void }) {
  const assigned = devices.filter((device) => rule.device_ids.includes(device.id))
  return <tr>
    <td><p className="font-medium">{rule.name}</p><p className="max-w-56 truncate text-xs text-muted-foreground">{rule.description || "No description"}</p></td>
    <td className="font-mono text-xs">{rule.gateway_ip}:{rule.gateway_port}<span className="mx-1.5 text-muted-foreground">→</span>{rule.backend_ip}:{rule.backend_port}</td>
    <td><span className="border px-2 py-0.5 font-mono text-[11px] uppercase">{rule.protocol}</span></td>
    <td>{rule.allow_all_peers ? <span className="text-sm">All active peers</span> : <div><p className="text-sm">{assigned.length} device{assigned.length === 1 ? "" : "s"}</p><p className="max-w-48 truncate text-xs text-muted-foreground">{assigned.map((device) => device.name).join(", ") || "None"}</p></div>}</td>
    <td><StatusBadge enabled={rule.enabled} /></td>
    <td><div className="flex justify-end gap-1"><Button size="icon-sm" variant="ghost" onClick={onEdit} aria-label={`Edit ${rule.name}`}><IconEdit /></Button><Button size="icon-sm" variant="ghost" className="text-destructive hover:text-destructive" onClick={onDelete} aria-label={`Delete ${rule.name}`}><IconTrash /></Button></div></td>
  </tr>
}

function StatusBadge({ enabled }: { enabled: boolean }) {
  return <span className={cn("inline-flex items-center gap-1.5 border px-2 py-0.5 text-xs", enabled ? "text-emerald-600" : "text-muted-foreground")}><span className={cn("size-1.5 rounded-full", enabled ? "bg-emerald-500" : "bg-muted-foreground")} />{enabled ? "Enabled" : "Disabled"}</span>
}

function RuleSheet({ open, onOpenChange, rule, devices, onSaved }: { open: boolean; onOpenChange: (open: boolean) => void; rule?: HostAccessRule; devices: HostAccessDevice[]; onSaved: () => void }) {
  const [form, setForm] = useState<SaveHostAccessRule>(() => rule ? toForm(rule) : newRule())
  useEffect(() => { if (open) setForm(rule ? toForm(rule) : newRule()) }, [open, rule])
  const save = useMutation({
    mutationFn: () => rule ? adminUpdateHostAccess(rule.id, form) : adminCreateHostAccess(form),
    onSuccess: () => { toast.success(rule ? "Host access rule updated" : "Host access rule created"); onSaved() },
    onError: (error: Error) => toast.error(error.message),
  })
  const set = <K extends keyof SaveHostAccessRule>(key: K, value: SaveHostAccessRule[K]) => setForm((current) => ({ ...current, [key]: value }))
  const valid = Boolean(form.name.trim() && form.gateway_port > 0 && form.backend_port > 0 && (form.allow_all_peers || form.device_ids.length > 0))

  return <Sheet open={open} onOpenChange={onOpenChange}>
    <SheetContent className="!w-full !max-w-none md:!w-[50vw]">
      <SheetHeader className="border-b pr-12"><SheetTitle>{rule ? "Edit host access rule" : "New host access rule"}</SheetTitle><SheetDescription>Changes are written to the database and applied to the live firewall immediately.</SheetDescription></SheetHeader>
      <div className="flex-1 space-y-5 overflow-y-auto px-4 pb-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Rule name"><Input value={form.name} onChange={(event) => set("name", event.target.value)} placeholder="Example: PostgreSQL" /></Field>
          <Field label="Protocol"><Select value={form.protocol} onValueChange={(value) => set("protocol", value as "tcp" | "udp")}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="tcp">TCP</SelectItem><SelectItem value="udp">UDP</SelectItem></SelectContent></Select></Field>
          <Field label="VPN gateway port"><Input type="number" min={1} max={65535} value={form.gateway_port || ""} onChange={(event) => set("gateway_port", Number(event.target.value))} /></Field>
          <Field label="Host service port"><Input type="number" min={1} max={65535} value={form.backend_port || ""} onChange={(event) => set("backend_port", Number(event.target.value))} /></Field>
        </div>
        <Field label="Description"><Input value={form.description} onChange={(event) => set("description", event.target.value)} placeholder="What this rule provides" /></Field>
        <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-2 border bg-muted/30 p-3 font-mono text-xs">
          <div><p className="mb-1 text-[10px] uppercase text-muted-foreground">VPN gateway</p><p>10.0.0.1:{form.gateway_port || "—"}</p></div><span className="text-muted-foreground">→</span><div><p className="mb-1 text-[10px] uppercase text-muted-foreground">Managed host</p><p>192.168.1.20:{form.backend_port || "—"}</p></div>
        </div>
        <ToggleRow title="Enabled" description="Apply this permission to the live firewall." checked={form.enabled} onCheckedChange={(value) => set("enabled", value)} />
        <ToggleRow title="All active VPN peers" description="Current and newly created peers automatically inherit access." checked={form.allow_all_peers} onCheckedChange={(value) => set("allow_all_peers", value)} />
        {!form.allow_all_peers && <Field label="Allowed devices"><DeviceMultiSelect devices={devices} selected={form.device_ids} onChange={(ids) => set("device_ids", ids)} /></Field>}
      </div>
      <SheetFooter className="flex-row justify-end border-t"><Button variant="outline" onClick={() => onOpenChange(false)} disabled={save.isPending}>Cancel</Button><Button onClick={() => save.mutate()} disabled={!valid || save.isPending}>{save.isPending ? "Applying…" : "Save and apply"}</Button></SheetFooter>
    </SheetContent>
  </Sheet>
}

function DeviceMultiSelect({ devices, selected, onChange }: { devices: HostAccessDevice[]; selected: string[]; onChange: (ids: string[]) => void }) {
  const [open, setOpen] = useState(false)
  const selectedDevices = useMemo(() => devices.filter((device) => selected.includes(device.id)), [devices, selected])
  const toggle = (id: string) => onChange(selected.includes(id) ? selected.filter((item) => item !== id) : [...selected, id])
  return <div className="space-y-2">
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild><Button variant="outline" className="w-full justify-between font-normal"><span className="inline-flex items-center gap-2"><IconDeviceLaptop className="size-4 text-muted-foreground" />{selected.length ? `${selected.length} device${selected.length === 1 ? "" : "s"} selected` : "Add devices"}</span><IconChevronDown className="size-4 text-muted-foreground" /></Button></PopoverTrigger>
      <PopoverContent align="start" className="w-[var(--radix-popover-trigger-width)] p-0">
        <Command><CommandInput placeholder="Search device, IP, or owner…" /><CommandList className="max-h-[min(24rem,50vh)] overflow-y-auto overscroll-contain" onWheel={(event) => event.stopPropagation()}><CommandEmpty>No devices found.</CommandEmpty><CommandGroup>
          {devices.map((device) => <CommandItem key={device.id} value={`${device.name} ${device.allocated_ip} ${device.owner_email}`} data-checked={selected.includes(device.id)} onSelect={() => toggle(device.id)}>
            <span className={cn("flex size-4 items-center justify-center border", selected.includes(device.id) && "border-primary bg-primary text-primary-foreground")}>{selected.includes(device.id) && <IconCheck className="size-3" />}</span>
            <span className="min-w-0"><span className="block truncate">{device.name} · {device.allocated_ip}</span><span className="block truncate text-xs text-muted-foreground">{device.owner_email}</span></span>
          </CommandItem>)}
        </CommandGroup></CommandList><div className="flex justify-between border-t p-2"><Button size="sm" variant="ghost" onClick={() => onChange([])} disabled={!selected.length}>Clear</Button><Button size="sm" onClick={() => setOpen(false)}>Done</Button></div></Command>
      </PopoverContent>
    </Popover>
    {selectedDevices.length > 0 && <div className="flex flex-wrap gap-1.5">{selectedDevices.map((device) => <span key={device.id} className="inline-flex items-center gap-1 border bg-muted/40 px-2 py-1 text-xs">{device.name}<span className="font-mono text-muted-foreground">{device.allocated_ip}</span><button type="button" onClick={() => toggle(device.id)} className="ml-0.5 text-muted-foreground hover:text-foreground" aria-label={`Remove ${device.name}`}><IconX className="size-3" /></button></span>)}</div>}
  </div>
}

function ToggleRow({ title, description, checked, onCheckedChange }: { title: string; description: string; checked: boolean; onCheckedChange: (checked: boolean) => void }) {
  return <div className="flex items-center justify-between gap-4 border p-3"><div><p className="text-sm font-medium">{title}</p><p className="text-xs text-muted-foreground">{description}</p></div><Switch checked={checked} onCheckedChange={onCheckedChange} /></div>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="flex flex-col gap-1.5"><Label className="zv-eyebrow">{label}</Label>{children}</div>
}

function toForm(rule: HostAccessRule): SaveHostAccessRule {
  return { name: rule.name, description: rule.description, protocol: rule.protocol, gateway_port: rule.gateway_port, backend_port: rule.backend_port, enabled: rule.enabled, allow_all_peers: rule.allow_all_peers, device_ids: [...rule.device_ids] }
}
