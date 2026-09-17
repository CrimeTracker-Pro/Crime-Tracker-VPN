import {
  IconCircleDashedX,
  IconClipboardList,
  IconDevices,
  IconHierarchy3,
  IconLayoutDashboard,
  IconLayoutSidebar,
  IconLayoutSidebarLeftCollapse,
  IconLogin2,
  IconRoute,
  IconRouter,
  IconSearch,
  IconSettings,
  IconUserShield,
  IconUsers,
} from "@tabler/icons-react"
import { Link, NavLink, useLocation } from "react-router"

import { MiniAreaChart } from "@/components/charts/LazyMiniAreaChart"
import { LiveDot, Logomark, Wordmark } from "@/components/swiss"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
  useSidebar,
} from "@/components/ui/sidebar"
import { cn } from "@/lib/utils"
import { useAuth } from "@/stores/auth"
import { formatBytes, formatBytesSI } from "@/lib/units"
import { useLiveStats } from "@/stores/liveStats"

type NavEntry = {
  to: string
  label: string
  icon: React.ComponentType<{ className?: string }>
  end?: boolean
}

const WORKSPACE: NavEntry[] = [
  { to: "/app", label: "Dashboard", icon: IconLayoutDashboard, end: true },
  { to: "/app/devices", label: "Devices", icon: IconDevices },
  { to: "/app/topology", label: "Topology", icon: IconHierarchy3 },
  { to: "/app/finder", label: "Finder", icon: IconSearch },
  { to: "/app/settings", label: "Settings", icon: IconSettings },
]

const ADMIN: NavEntry[] = [
  { to: "/admin", label: "Overview", icon: IconUserShield, end: true },
  { to: "/admin/users", label: "Users", icon: IconUsers },
  { to: "/admin/audit", label: "Audit log", icon: IconClipboardList },
  { to: "/admin/sessions", label: "Sessions", icon: IconLogin2 },
  { to: "/admin/access-logs", label: "Access logs", icon: IconRoute },
  { to: "/admin/failed-logins", label: "Failed logins", icon: IconCircleDashedX },
  { to: "/admin/servers", label: "Servers", icon: IconRouter },
  { to: "/admin/topology", label: "Topology", icon: IconHierarchy3 },
  { to: "/admin/finder", label: "Finder", icon: IconSearch },
]

export function AppSidebar() {
  const user = useAuth((s) => s.user)
  const { state, isMobile, setOpenMobile } = useSidebar()
  const collapsed = state === "collapsed"

  return (
    // `dark` is applied to the sidebar root unconditionally so the
    // sidebar surface stays dark even when the app is in light mode.
    // Tailwind's `dark` variant (see index.css: `@custom-variant dark
    // (&:is(.dark *))`) makes every descendant resolve text-foreground /
    // border-border / etc against the dark palette — without this,
    // `text-foreground` inside ServerStats would be #0a0a0a on a dark
    // background and become invisible.
    <Sidebar collapsible="icon" className="dark border-r">
      <SidebarHeader
        className={cn(
          "border-sidebar-border h-12 justify-center border-b py-0",
          // Collapsed sidebar narrows to icon-width; drop the horizontal
          // padding so the logomark stays centered instead of getting
          // clipped against the right edge.
          collapsed ? "px-0" : "px-4",
        )}
      >
        <Link
          to="/app"
          onClick={() => {
            if (isMobile) setOpenMobile(false)
          }}
          className="flex h-12 items-center justify-center font-mono text-xs font-medium tracking-[0.04em]"
          aria-label="Crime Tracker VPN"
        >
          {collapsed ? <Logomark size={18} /> : <Wordmark size={12} />}
        </Link>
      </SidebarHeader>

      <SidebarContent className="gap-0">
        <SidebarGroup>
          <SidebarGroupLabel className="font-mono text-[10px] uppercase tracking-[0.1em]">
            Workspace
          </SidebarGroupLabel>
          <SidebarGroupContent>
            <NavList entries={WORKSPACE} />
          </SidebarGroupContent>
        </SidebarGroup>

        {user?.role === "admin" && (
          <SidebarGroup>
            <SidebarGroupLabel className="font-mono text-[10px] uppercase tracking-[0.1em]">
              Admin
            </SidebarGroupLabel>
            <SidebarGroupContent>
              <NavList entries={ADMIN} />
            </SidebarGroupContent>
          </SidebarGroup>
        )}
      </SidebarContent>

      <SidebarFooter className="gap-0">
        {!collapsed && <ServerStats />}
        <CollapseToggle collapsed={collapsed} />
      </SidebarFooter>

      <SidebarRail />
    </Sidebar>
  )
}

function NavList({ entries }: { entries: NavEntry[] }) {
  const location = useLocation()
  // On mobile the sidebar renders as a Sheet (drawer). Tapping a nav
  // item navigates but leaves the drawer covering the destination page,
  // forcing a manual second tap to close. Auto-dismiss the drawer on
  // navigation — only on mobile, since desktop keeps the rail visible.
  const { isMobile, setOpenMobile } = useSidebar()
  const closeIfMobile = () => {
    if (isMobile) setOpenMobile(false)
  }
  return (
    <SidebarMenu>
      {entries.map((entry) => {
        const Icon = entry.icon
        const isActive = entry.end
          ? location.pathname === entry.to
          : location.pathname.startsWith(entry.to)
        return (
          <SidebarMenuItem key={entry.to}>
            <SidebarMenuButton
              asChild
              isActive={isActive}
              tooltip={entry.label}
              className={cn(
                "relative h-8 rounded-none text-[13px]",
                isActive &&
                  // Accent left bar. The active item's bg-tint + accent
                  // text/icon are applied via the
                  // [data-sidebar=menu-button][data-active=true] rule in
                  // index.css (reliably beats the base gray + tracks --primary).
                  "font-medium before:bg-primary before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-0.5 before:content-['']",
              )}
            >
              <NavLink to={entry.to} end={entry.end} onClick={closeIfMobile}>
                <Icon className="size-4" />
                <span>{entry.label}</span>
              </NavLink>
            </SidebarMenuButton>
          </SidebarMenuItem>
        )
      })}
    </SidebarMenu>
  )
}

/**
 * Server-stats panel for the sidebar footer. Renders host-level metrics
 * from the worker's server_health emitter: CPU%, memory used/total,
 * disk I/O sparkline, net I/O, and uptime. Visible to all signed-in
 * users so they can see the server's current load.
 *
 * If there's no server_health event yet (worker not started, or just
 * after first boot), shows a "Waiting" placeholder — the chart fills
 * in within 5 seconds of the worker coming up.
 */
function ServerStats() {
  const health = useLiveStats((s) => {
    const ids = Object.keys(s.serverHealth)
    return ids.length > 0 ? s.serverHealth[ids[0]] : null
  })

  // Distinguish "haven't received an event yet" (panel waits, no zeros)
  // from "events arriving, host genuinely idle" (zeros render). The
  // worker emits server_health every 5 s; if you've been on the page
  // for >10 s with no values, the worker binary is most likely an old
  // build without the server_health module — rebuild it.
  if (!health || health.lastTs === 0) {
    return (
      <div className="mx-2 mb-1 space-y-1.5">
        <div className="flex items-center justify-between font-mono text-[10px] uppercase tracking-[0.08em]">
          <span className="text-muted-foreground">Server stats</span>
          <span className="text-muted-foreground inline-flex items-center gap-1.5">
            <LiveDot />
            Waiting
          </span>
        </div>
        <p className="text-muted-foreground/70 py-2 font-mono text-[10px] leading-snug">
          No server_health event yet — worker emits every 5 s. If this
          persists, the worker binary is missing the new emitter
          (rebuild required).
        </p>
      </div>
    )
  }

  const cpuPct = health.cpuPct
  const memUsed = health.memUsedBytes
  const memTotal = health.memTotalBytes
  const memPct = memTotal > 0 ? (memUsed / memTotal) * 100 : 0
  // "Real I/O" = wg0 tunnel rate. Worker computes (cur - prev) / interval
  // each tick from the cumulative `/sys/class/net/wg0/statistics` counters
  // (or Docker stats' `networks.wg0` when running in the dev compose), so
  // these are already per-second values — no further conversion needed.
  const wgRx = health.wgRxBps
  const wgTx = health.wgTxBps
  // Net I/O is the cumulative-since-container-start figure straight from
  // `docker stats <name>` — not a per-second rate. We render it verbatim
  // (e.g. `↓ 39.4 MB · ↑ 13.1 MB`) so the sidebar matches what an operator
  // sees on the host.
  const netRxTotal = health.netRxTotalBytes
  const netTxTotal = health.netTxTotalBytes

  return (
    <div className="mx-2 mb-1 space-y-1.5">
      <div className="flex items-center justify-between font-mono text-[10px] uppercase tracking-[0.08em]">
        <span className="text-muted-foreground">Server stats</span>
        <span className="text-muted-foreground inline-flex items-center gap-1.5">
          <LiveDot />
          Host
        </span>
      </div>

      <ProgressRow label="CPU" pct={cpuPct} value={`${cpuPct.toFixed(2)}%`} />
      <ProgressRow
        label="Mem"
        pct={memPct}
        value={`${formatBytes(memUsed)} / ${formatBytes(memTotal)}`}
      />

      <div className="pt-1">
        {/* Real I/O = wg0 tunnel rate. Title + ↓/↑ rate share the row so
            the numbers sit in the corner, matching the Net I/O row below.
            Worker already gives us bytes/sec; format as a byte rate (no
            ÷8 since wg0 statistics are in bytes, not bits). */}
        <div className="flex items-center justify-between pb-0.5 font-mono text-[10px] tabular-nums">
          <span className="text-muted-foreground uppercase tracking-[0.08em]">
            Real I/O · wg0
          </span>
          <span className="text-foreground">
            <span className="text-primary">↓</span> {formatBytes(wgRx)}/s
            <span className="text-muted-foreground px-1">·</span>
            <span className="text-primary">↑</span> {formatBytes(wgTx)}/s
          </span>
        </div>
        <MiniAreaChart
          rxHistory={health.wgRxHistory}
          txHistory={health.wgTxHistory}
          height={32}
        />
      </div>

      <div className="pt-1">
        <div className="text-muted-foreground flex items-center justify-between font-mono text-[10px] tabular-nums">
          <span className="uppercase tracking-[0.08em]">Net I/O</span>
          <span className="text-foreground">
            {/* Cumulative bytes-since-container-start, summed across every
                interface. 1:1 with the "Net I/O" column from
                `docker stats <name>` — no `/s` suffix since the figure
                isn't a rate. `formatBytesSI` (decimal kB/MB/GB) is what
                `docker stats` uses for that column; the binary
                `formatBytes` would round ~5% smaller and make the
                sidebar look out of sync with the CLI. */}
            <span className="text-primary">↓</span> {formatBytesSI(netRxTotal)}
            <span className="text-muted-foreground px-1">·</span>
            <span className="text-primary">↑</span> {formatBytesSI(netTxTotal)}
          </span>
        </div>
      </div>

      <div className="text-muted-foreground flex items-center justify-between pt-1 font-mono text-[10px] tabular-nums">
        <span>Uptime</span>
        <span className="text-foreground">
          {formatUptime(health.uptimeSec)}
        </span>
      </div>
    </div>
  )
}

/** Inline label · bar · value row used inside ServerStats. Bar color +
 * value color tier on the percentage so a glance at the sidebar reads
 * "healthy / warning / critical" without having to parse the number:
 *   < 60 % → online (green)
 *   60–85 % → degraded (amber)
 *   > 85 % → revoked (red)
 * Matches the tone scheme used by StatusPill elsewhere in the app. */
function ProgressRow({
  label,
  pct,
  value,
}: {
  label: string
  pct: number
  value: string
}) {
  const clamped = Math.max(0, Math.min(100, pct))
  const tone =
    clamped > 85 ? "revoked" : clamped > 60 ? "degraded" : "online"
  const fillClass =
    tone === "revoked"
      ? "bg-status-revoked"
      : tone === "degraded"
        ? "bg-status-degraded"
        : "bg-status-online"
  const valueClass =
    tone === "revoked"
      ? "text-status-revoked"
      : tone === "degraded"
        ? "text-status-degraded"
        : "text-foreground"
  return (
    <div className="space-y-0.5">
      <div className="text-muted-foreground flex items-center justify-between font-mono text-[10px] tabular-nums">
        <span>{label}</span>
        <span className={valueClass}>{value}</span>
      </div>
      <div className="bg-muted/30 h-1 w-full overflow-hidden">
        <div
          className={cn(
            "h-full transition-[width,background-color] duration-300 ease-out",
            fillClass,
          )}
          style={{ width: `${clamped}%` }}
        />
      </div>
    </div>
  )
}

/** Format a duration in seconds as "dd hh mm ss". Days drop when 0 to
 * keep the line short during the first day of uptime. */
function formatUptime(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) return "—"
  const d = Math.floor(sec / 86400)
  const h = Math.floor((sec % 86400) / 3600)
  const m = Math.floor((sec % 3600) / 60)
  const s = Math.floor(sec % 60)
  const pad = (n: number) => n.toString().padStart(2, "0")
  if (d > 0) return `${d}d ${pad(h)}h ${pad(m)}m ${pad(s)}s`
  return `${pad(h)}h ${pad(m)}m ${pad(s)}s`
}

/**
 * Sidebar collapse/expand toggle. Lives in the footer where the theme
 * picker used to — the theme + user-menu both moved to the top bar.
 */
function CollapseToggle({ collapsed }: { collapsed: boolean }) {
  const { toggleSidebar } = useSidebar()
  const Icon = collapsed ? IconLayoutSidebar : IconLayoutSidebarLeftCollapse
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton
          onClick={toggleSidebar}
          tooltip={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          className="h-8 rounded-none text-[13px]"
        >
          <Icon className="size-4" />
          <span>{collapsed ? "Expand" : "Collapse"}</span>
        </SidebarMenuButton>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}
