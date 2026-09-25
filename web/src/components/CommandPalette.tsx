import {
  IconClipboardList,
  IconCircleDashedX,
  IconDevices,
  IconHierarchy3,
  IconLayoutDashboard,
  IconLogin2,
  IconRoute,
  IconRouter,
  IconSearch,
  IconSettings,
  IconUserShield,
} from "@tabler/icons-react"
import { useEffect, useState } from "react"
import { useNavigate } from "react-router"

import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from "@/components/ui/command"
import { useAuth } from "@/stores/auth"

type Item = {
  to: string
  label: string
  icon: React.ComponentType<{ className?: string }>
  group: "Workspace" | "Admin"
  /** Linear-style chord nav, e.g. "g d" */
  chord?: string
}

const ITEMS: Item[] = [
  {
    to: "/app",
    label: "Dashboard",
    icon: IconLayoutDashboard,
    group: "Workspace",
    chord: "g d",
  },
  { to: "/app/devices", label: "Devices", icon: IconDevices, group: "Workspace" },
  {
    to: "/app/topology",
    label: "Topology",
    icon: IconHierarchy3,
    group: "Workspace",
    chord: "g t",
  },
  {
    to: "/app/settings",
    label: "Settings",
    icon: IconSettings,
    group: "Workspace",
  },
  {
    to: "/app/finder",
    label: "Finder",
    icon: IconSearch,
    group: "Workspace",
    chord: "g f",
  },

  // Admin
  {
    to: "/admin",
    label: "Admin overview",
    icon: IconUserShield,
    group: "Admin",
    chord: "g a",
  },
  { to: "/admin/servers", label: "Servers", icon: IconRouter, group: "Admin" },
  {
    to: "/admin/topology",
    label: "Topology (fleet)",
    icon: IconHierarchy3,
    group: "Admin",
  },
  {
    to: "/admin/audit",
    label: "Audit log",
    icon: IconClipboardList,
    group: "Admin",
  },
  {
    to: "/admin/sessions",
    label: "Sessions",
    icon: IconLogin2,
    group: "Admin",
  },
  {
    to: "/admin/access-logs",
    label: "Access logs",
    icon: IconRoute,
    group: "Admin",
  },
  {
    to: "/admin/failed-logins",
    label: "Failed logins",
    icon: IconCircleDashedX,
    group: "Admin",
  },
]

export function CommandPalette({
  openOverride,
  setOpenOverride,
}: {
  openOverride?: boolean
  setOpenOverride?: (next: boolean) => void
} = {}) {
  const [internalOpen, setInternalOpen] = useState(false)
  const open = openOverride ?? internalOpen
  const setOpen = setOpenOverride ?? setInternalOpen
  const navigate = useNavigate()
  const user = useAuth((s) => s.user)
  const isAdmin = user?.role === "admin"

  // Cmd/Ctrl + K → open. Lowercase ? → also open ("help").
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.key === "k" || e.key === "K") && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        setOpen(!open)
        return
      }
      if (
        e.key === "?" &&
        !(e.metaKey || e.ctrlKey || e.altKey) &&
        !isEditableTarget(e.target)
      ) {
        e.preventDefault()
        setOpen(true)
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [open, setOpen])

  // Linear-style chord nav: "g d" → /app, "g s" → /admin/servers, etc.
  // Buffered chord with a 750 ms timeout.
  useEffect(() => {
    let buf = ""
    let timer: number | undefined
    function reset() {
      buf = ""
      if (timer) {
        window.clearTimeout(timer)
        timer = undefined
      }
    }
    function onKey(e: KeyboardEvent) {
      if (
        e.metaKey ||
        e.ctrlKey ||
        e.altKey ||
        isEditableTarget(e.target) ||
        open
      )
        return
      const k = e.key.toLowerCase()
      if (k.length !== 1) return
      buf = (buf + k).slice(-3)
      if (timer) window.clearTimeout(timer)
      timer = window.setTimeout(reset, 750)
      const m = buf.match(/g([a-z])$/)
      if (!m) return
      const chord = `g ${m[1]}`
      const item = ITEMS.find((i) => i.chord === chord)
      if (!item) return
      if (item.group === "Admin" && !isAdmin) return
      e.preventDefault()
      reset()
      navigate(item.to)
    }
    window.addEventListener("keydown", onKey)
    return () => {
      window.removeEventListener("keydown", onKey)
      reset()
    }
  }, [navigate, isAdmin, open])

  function go(to: string) {
    setOpen(false)
    navigate(to)
  }

  const grouped = ITEMS.reduce<Record<string, Item[]>>((acc, item) => {
    if (item.group === "Admin" && !isAdmin) return acc
    ;(acc[item.group] ??= []).push(item)
    return acc
  }, {})

  return (
    <CommandDialog open={open} onOpenChange={setOpen} title="Command palette">
      <CommandInput placeholder="Type a command or search…" />
      <CommandList>
        <CommandEmpty>No results found.</CommandEmpty>
        {Object.entries(grouped).map(([group, items], idx) => (
          <div key={group}>
            {idx > 0 && <CommandSeparator />}
            <CommandGroup heading={group}>
              {items.map((item) => {
                const Icon = item.icon
                return (
                  <CommandItem
                    key={item.to}
                    value={`${item.label} ${item.to}`}
                    onSelect={() => go(item.to)}
                  >
                    <Icon />
                    <span>{item.label}</span>
                    {item.chord && (
                      <span className="zv-kbd ml-auto">{item.chord}</span>
                    )}
                  </CommandItem>
                )
              })}
            </CommandGroup>
          </div>
        ))}
      </CommandList>
    </CommandDialog>
  )
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  if (target.closest("input, textarea, select, [contenteditable='true']"))
    return true
  return false
}
