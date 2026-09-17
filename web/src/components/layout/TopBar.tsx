import { IconArrowLeft, IconSearch, IconUserX } from "@tabler/icons-react"
import { useEffect, useState } from "react"
import { Link, useMatches, useNavigate } from "react-router"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { UserMenu } from "@/components/layout/UserMenu"
import { ModeToggle } from "@/components/mode-toggle"
import { Kbd, LiveDot } from "@/components/swiss"
import { SidebarTrigger } from "@/components/ui/sidebar"
import { WithTooltip } from "@/components/ui/with-tooltip"
import { useBreadcrumbStore } from "@/stores/breadcrumb"
import { adminStopImpersonation, me } from "@/lib/api"
import { useAuth } from "@/stores/auth"

type Crumb = { to?: string; label: string }
type CrumbHandle = {
  breadcrumb?: string | ((params: Record<string, string>) => string)
  /** Declarative parent crumbs for pages whose URL doesn't reflect their
   *  logical parent (e.g. /app/devices/:id sits flat under the layout but
   *  conceptually belongs under /app/devices). Rendered before the
   *  match's own crumb. */
  parents?: { label: string; to: string }[]
}

function isCrumbHandle(value: unknown): value is CrumbHandle {
  return (
    typeof value === "object" &&
    value !== null &&
    ("breadcrumb" in value || "parents" in value)
  )
}

function useBreadcrumbs(): Crumb[] {
  const matches = useMatches()
  const overrides = useBreadcrumbStore((s) => s.overrides)
  const crumbs: Crumb[] = []
  for (const m of matches) {
    if (!isCrumbHandle(m.handle)) continue
    if (m.handle.parents) {
      for (const p of m.handle.parents) {
        if (crumbs.some((c) => c.to === p.to)) continue
        crumbs.push({ to: p.to, label: p.label })
      }
    }
    const b = m.handle.breadcrumb
    if (b == null) continue
    const fallback =
      typeof b === "function"
        ? b((m.params ?? {}) as Record<string, string>)
        : b
    const label = overrides[m.id] ?? fallback
    if (!label) continue
    crumbs.push({ to: m.pathname, label })
  }
  return crumbs
}

export function TopBar({
  onOpenCommand,
}: {
  onOpenCommand?: () => void
}) {
  const crumbs = useBreadcrumbs()
  const user = useAuth((s) => s.user)
  const setUser = useAuth((s) => s.setUser)
  const navigate = useNavigate()
  const [stopping, setStopping] = useState(false)
  const [stopImpersonationConfirmOpen, setStopImpersonationConfirmOpen] =
    useState(false)

  const handleStopImpersonation = async () => {
    setStopping(true)
    try {
      await adminStopImpersonation()
      const updated = await me()
      setUser(updated)
      // Close the confirm dialog before navigating — the TopBar stays mounted
      // across the move to /admin/users, so without this the dialog lingers.
      setStopImpersonationConfirmOpen(false)
      toast.success("Returned to your admin session")
      void navigate("/admin/users")
    } catch {
      toast.error("Failed to stop impersonation")
    } finally {
      setStopping(false)
    }
  }

  return (
    <header className="bg-background sticky top-0 z-30 border-b">
      {user?.is_impersonated && (
        <div className="flex items-center gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5">
          <IconUserX className="size-3.5 shrink-0 text-amber-600 dark:text-amber-400" />
          <span className="font-mono text-[11px] text-amber-700 dark:text-amber-400">
            Impersonating{" "}
            <span className="font-semibold">{user.email}</span>
            {user.impersonator_email && (
              <span className="text-amber-600/70 dark:text-amber-500/70">
                {" "}· admin: {user.impersonator_email}
              </span>
            )}
          </span>
          <button
            type="button"
            disabled={stopping}
            onClick={() => setStopImpersonationConfirmOpen(true)}
            className="ml-auto inline-flex items-center gap-1 border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 font-mono text-[10px] text-amber-700 transition-colors hover:bg-amber-500/20 disabled:opacity-50 dark:text-amber-400"
          >
            {stopping ? "Stopping…" : "Exit impersonation →"}
          </button>
        </div>
      )}
      <div className="flex h-12 items-center gap-3 px-4">
        <SidebarTrigger className="md:hidden" />
        <BackButton parentTo={parentOfLeaf(crumbs)} />
        <nav
          aria-label="breadcrumb"
          className="text-muted-foreground flex min-w-0 items-center gap-2 font-mono text-[12px]"
        >
          {crumbs.length === 0 ? (
            <span className="text-foreground truncate font-medium">Crime Tracker VPN</span>
          ) : (
            crumbs.map((c, i) => {
              const last = i === crumbs.length - 1
              return (
                <span key={`${c.to}-${i}`} className="flex items-center gap-2">
                  {i > 0 && (
                    <span className="text-muted-foreground/50 shrink-0">/</span>
                  )}
                  {last || !c.to ? (
                    <span className="text-foreground truncate font-medium">
                      {c.label}
                    </span>
                  ) : (
                    <Link
                      to={c.to}
                      className="hover:text-foreground truncate transition-colors"
                    >
                      {c.label}
                    </Link>
                  )}
                </span>
              )
            })
          )}
        </nav>

        <div className="ml-auto flex items-center gap-2">
          <SearchTrigger onClick={onOpenCommand} />
          <LivePill />
          <ModeToggle />
          <UserMenu />
        </div>
      </div>

      <ConfirmDialog
        open={stopImpersonationConfirmOpen}
        onOpenChange={setStopImpersonationConfirmOpen}
        title="Exit impersonation?"
        description="You'll return to your admin session and leave this user's view."
        confirmLabel="Exit impersonation"
        cancelLabel="Keep impersonating"
        pending={stopping}
        onConfirm={() => void handleStopImpersonation()}
      />
    </header>
  )
}

/** The crumb immediately before the leaf — i.e. the declared parent of
 *  the current page. Drives the back button: only present on child
 *  pages (any route that declared `parents` in its handle), absent on
 *  top-level pages so the button hides entirely. */
function parentOfLeaf(crumbs: Crumb[]): string | undefined {
  if (crumbs.length < 2) return undefined
  return crumbs[crumbs.length - 2]?.to
}

/** Top-left back affordance. Renders only when the current page has a
 *  declared parent crumb. Always navigates straight to that parent —
 *  never `navigate(-1)`, so the destination is stable regardless of how
 *  the user arrived (deep link, in-app navigation, or refresh). */
function BackButton({ parentTo }: { parentTo?: string }) {
  const navigate = useNavigate()
  if (!parentTo) return null
  return (
    <WithTooltip label="Back to parent" side="bottom">
      <button
        type="button"
        onClick={() => navigate(parentTo)}
        aria-label="Back to parent"
        className="text-muted-foreground hover:text-foreground border-border hover:border-foreground inline-flex size-7 shrink-0 items-center justify-center border transition-colors"
      >
        <IconArrowLeft className="size-3.5" />
      </button>
    </WithTooltip>
  )
}

function SearchTrigger({ onClick }: { onClick?: () => void }) {
  return (
    <WithTooltip label="Search · jump (⌘K)" side="bottom">
      <button
        type="button"
        onClick={onClick}
        className="text-muted-foreground border-border bg-background hover:border-foreground focus-visible:ring-ring focus-visible:border-foreground flex h-7 w-7 items-center justify-center gap-2 border text-xs transition-colors focus-visible:outline-none focus-visible:ring-1 sm:w-44 sm:justify-start sm:px-2.5 md:w-64"
        aria-label="Open search"
      >
        <IconSearch className="size-3.5 shrink-0" />
        <span className="hidden flex-1 text-left sm:inline">Search · jump…</span>
        <Kbd className="hidden md:inline-flex">⌘K</Kbd>
      </button>
    </WithTooltip>
  )
}

/** Live status pill — pulsing dot + UTC clock. Matches the design's
 * topbar live-pill exactly. Updates every second on a local interval. */
function LivePill() {
  const [t, setT] = useState(() => new Date())
  useEffect(() => {
    const id = setInterval(() => setT(new Date()), 1000)
    return () => clearInterval(id)
  }, [])
  const pad = (n: number) => String(n).padStart(2, "0")
  const time = `${pad(t.getUTCHours())}:${pad(t.getUTCMinutes())}:${pad(t.getUTCSeconds())} UTC`
  return (
    <span className="border-border text-muted-foreground hidden h-7 items-center gap-1.5 border px-2 font-mono text-[11px] md:inline-flex">
      <LiveDot />
      <span>connected</span>
      <span className="text-muted-foreground/70">· {time}</span>
    </span>
  )
}
