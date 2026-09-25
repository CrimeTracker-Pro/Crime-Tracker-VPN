import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"

/**
 * Suspense fallbacks for the protected (DashboardLayout) routes. Each
 * skeleton mirrors the geometry of the actual page it stands in for
 * (PageHead → KpiStrip → Panels) so there's no layout shift when the
 * lazy chunk resolves.
 *
 * Use the `<RouteSkeleton pathname=...>` switcher from DashboardLayout's
 * Suspense fallback — it picks the right shape per route.
 */

// ── Atoms ─────────────────────────────────────────────────────────────

function SkPageHead({ right }: { right?: "button" | "buttons" | "search" }) {
  return (
    <div className="zv-page-head">
      <div className="flex min-w-0 flex-col gap-1.5">
        <Skeleton className="h-2.5 w-24 rounded-none" />
        <Skeleton className="h-7 w-44 rounded-none" />
      </div>
      {right === "button" && (
        <Skeleton className="h-8 w-32 rounded-md shrink-0" />
      )}
      {right === "buttons" && (
        <div className="flex items-center gap-2 shrink-0">
          <Skeleton className="h-7 w-20 rounded-md" />
          <Skeleton className="h-7 w-20 rounded-md" />
        </div>
      )}
      {right === "search" && (
        <Skeleton className="h-8 w-64 rounded-md shrink-0" />
      )}
    </div>
  )
}

function SkKpiStrip({ count = 4 }: { count?: number }) {
  return (
    <div className="zv-kpi-strip">
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="zv-kpi" style={{ gap: 6 }}>
          <Skeleton className="h-2.5 w-20 rounded-none" />
          <Skeleton className="mt-1 h-7 w-24 rounded-none" />
          <Skeleton className="h-[26px] w-full rounded-none" />
          <div className="flex items-center justify-between">
            <Skeleton className="h-2.5 w-16 rounded-none" />
            <Skeleton className="h-2.5 w-10 rounded-none" />
          </div>
        </div>
      ))}
    </div>
  )
}

function SkPanel({
  children,
  headRight = true,
  sub = true,
  className,
  flush,
  bodyClassName,
}: {
  children: React.ReactNode
  headRight?: boolean
  sub?: boolean
  className?: string
  flush?: boolean
  bodyClassName?: string
}) {
  return (
    <div className={cn("zv-panel", className)}>
      <div className="zv-panel-head">
        <div className="flex min-w-0 flex-col gap-1">
          <Skeleton className="h-3 w-32 rounded-none" />
          {sub && <Skeleton className="h-2.5 w-44 rounded-none" />}
        </div>
        {headRight && <Skeleton className="h-6 w-20 rounded-md shrink-0" />}
      </div>
      <div className={cn("zv-panel-body", flush && "flush", bodyClassName)}>
        {children}
      </div>
    </div>
  )
}

function SkTableRows({ rows = 6, cols = 6 }: { rows?: number; cols?: number }) {
  // Mock-table look — alternating row widths so it feels like real data
  // rather than identical bars. Uses border lines for the row rhythm so
  // it matches the .zv-table cell heights when the real table mounts.
  return (
    <div className="flex flex-col">
      <div className="border-border flex items-center gap-4 border-b px-4 py-2.5">
        {Array.from({ length: cols }).map((_, i) => (
          <Skeleton
            key={i}
            className="h-2.5 rounded-none"
            style={{ width: i === 0 ? "20%" : "12%" }}
          />
        ))}
      </div>
      {Array.from({ length: rows }).map((_, r) => (
        <div
          key={r}
          className="border-border flex items-center gap-4 border-b px-4 py-3"
        >
          {Array.from({ length: cols }).map((_, c) => (
            <Skeleton
              key={c}
              className="h-3 rounded-none"
              style={{ width: c === 0 ? `${28 - r}%` : `${14 - c}%` }}
            />
          ))}
        </div>
      ))}
    </div>
  )
}

function SkActivityRows({ rows = 5 }: { rows?: number }) {
  return (
    <div className="flex flex-col gap-3">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="flex items-center gap-3">
          <Skeleton className="size-2 rounded-full" />
          <Skeleton
            className="h-3 rounded-none"
            style={{ width: `${72 - i * 6}%` }}
          />
          <Skeleton className="ml-auto h-2.5 w-12 rounded-none" />
        </div>
      ))}
    </div>
  )
}

// ── Page-specific skeletons ───────────────────────────────────────────

/** /app — Dashboard */
function DashboardSkeleton() {
  return (
    <>
      <SkPageHead />
      <SkKpiStrip />
      <SkPanel>
        <Skeleton className="h-[220px] w-full rounded-none" />
      </SkPanel>
      <div className="grid gap-6 lg:grid-cols-2">
        <SkPanel flush bodyClassName="px-4 py-4">
          <SkActivityRows rows={6} />
        </SkPanel>
        <SkPanel flush bodyClassName="px-4 py-4">
          <SkActivityRows rows={6} />
        </SkPanel>
      </div>
    </>
  )
}

/** /app/devices — Devices list */
function DevicesSkeleton() {
  return (
    <>
      <SkPageHead right="button" />
      <div className="zv-panel">
        <div className="grid grid-cols-1 gap-0 md:grid-cols-3">
          {Array.from({ length: 3 }).map((_, i) => (
            <div
              key={i}
              className={cn(
                "border-border flex flex-col gap-3 p-5",
                i < 2 && "md:border-r",
              )}
            >
              <Skeleton className="h-2.5 w-24 rounded-none" />
              <Skeleton className="h-10 w-20 rounded-none" />
              <div className="flex gap-3">
                <Skeleton className="h-2.5 w-16 rounded-none" />
                <Skeleton className="h-2.5 w-16 rounded-none" />
                <Skeleton className="h-2.5 w-16 rounded-none" />
              </div>
            </div>
          ))}
        </div>
      </div>
      <div className="zv-panel">
        <div className="zv-panel-head">
          <div className="flex items-center gap-2">
            <Skeleton className="h-6 w-16 rounded-md" />
            <Skeleton className="h-6 w-20 rounded-md" />
            <Skeleton className="h-6 w-16 rounded-md" />
          </div>
          <Skeleton className="h-8 w-48 rounded-md shrink-0" />
        </div>
        <SkTableRows rows={6} cols={7} />
      </div>
    </>
  )
}

/** /app/devices/:id — Device detail */
function DeviceDetailSkeleton() {
  return (
    <>
      <div className="zv-page-head">
        <div className="flex min-w-0 flex-col gap-1.5">
          <Skeleton className="h-2.5 w-36 rounded-none" />
          <Skeleton className="h-7 w-56 rounded-none" />
          <Skeleton className="mt-1 h-2.5 w-72 rounded-none" />
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <Skeleton className="h-6 w-14 rounded-md" />
          <Skeleton className="h-7 w-16 rounded-md" />
          <Skeleton className="h-7 w-16 rounded-md" />
          <Skeleton className="h-7 w-16 rounded-md" />
        </div>
      </div>
      <SkKpiStrip />
      <div className="grid gap-6 lg:grid-cols-[1.4fr_1fr]">
        <SkPanel>
          <Skeleton className="h-[220px] w-full rounded-none" />
        </SkPanel>
        <SkPanel>
          <div className="flex flex-col gap-3">
            {Array.from({ length: 6 }).map((_, i) => (
              <div key={i} className="flex items-baseline justify-between gap-3">
                <Skeleton className="h-3 w-20 rounded-none" />
                <Skeleton
                  className="h-3 rounded-none"
                  style={{ width: `${50 - i * 4}%` }}
                />
              </div>
            ))}
          </div>
        </SkPanel>
      </div>
      <SkPanel>
        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-7 w-full rounded-md" />
          ))}
        </div>
      </SkPanel>
      <SkPanel>
        <SkActivityRows rows={6} />
      </SkPanel>
    </>
  )
}

/** /app/topology — Topology
 *
 * Mirrors the real `<LiveTopology>` geometry: a full-bleed SVG canvas
 * inside a `!p-0 !h-[calc(100vh-220px)]` panel body, hairline grid
 * background, a center hub, three user-tier nodes around it, peer
 * nodes radiating from each user, hairline edges connecting them, a
 * HUD readout top-right, and the bottom-right pan/zoom toolbar.
 *
 * The whole node graph is `animate-pulse`'d so it reads as a loading
 * state without using individual <Skeleton> divs (which would lay out
 * separately and not align with the SVG coordinate system).
 */
function TopologySkeleton() {
  // Layout: 800x500 viewBox, hub at center, 3 user nodes on a ring,
  // each user has 2-3 peer nodes radiating outward. Hand-positioned so
  // the rest state matches what LiveTopology typically renders.
  const HUB = { x: 400, y: 250 }
  const users = [
    { x: 250, y: 170, peers: [{ x: 110, y: 110 }, { x: 90, y: 200 }] },
    { x: 250, y: 340, peers: [{ x: 90, y: 320 }, { x: 110, y: 410 }] },
    { x: 560, y: 250, peers: [{ x: 700, y: 160 }, { x: 720, y: 260 }, { x: 700, y: 360 }] },
  ]
  return (
    <>
      <SkPageHead />
      <div className="zv-panel">
        <div className="zv-panel-head">
          <div className="flex min-w-0 flex-col gap-1">
            <Skeleton className="h-3 w-28 rounded-none" />
            <Skeleton className="h-2.5 w-20 rounded-none" />
          </div>
          <Skeleton className="h-6 w-16 rounded-md" />
        </div>
        <div className="zv-panel-body flush relative h-[calc(100vh-220px)] min-h-[480px] overflow-hidden">
          <svg
            viewBox="0 0 800 500"
            preserveAspectRatio="xMidYMid meet"
            className="absolute inset-0 size-full text-border"
            aria-hidden
          >
            <defs>
              <pattern
                id="zv-topo-skel-grid"
                width="40"
                height="40"
                patternUnits="userSpaceOnUse"
              >
                <path
                  d="M 40 0 L 0 0 0 40"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="0.5"
                />
              </pattern>
            </defs>
            <rect
              width="800"
              height="500"
              fill="url(#zv-topo-skel-grid)"
              opacity="0.55"
            />

            <g className="animate-pulse text-muted-foreground/40">
              {/* hub → user edges */}
              {users.map((u, i) => (
                <line
                  key={`hu-${i}`}
                  x1={HUB.x}
                  y1={HUB.y}
                  x2={u.x}
                  y2={u.y}
                  stroke="currentColor"
                  strokeWidth="1.1"
                />
              ))}

              {/* user → peer edges */}
              {users.flatMap((u, ui) =>
                u.peers.map((p, pi) => (
                  <line
                    key={`up-${ui}-${pi}`}
                    x1={u.x}
                    y1={u.y}
                    x2={p.x}
                    y2={p.y}
                    stroke="currentColor"
                    strokeWidth="0.8"
                    opacity="0.7"
                  />
                )),
              )}

              {/* peer nodes — small rounded squares to match real peers */}
              {users.flatMap((u, ui) =>
                u.peers.map((p, pi) => (
                  <g key={`pn-${ui}-${pi}`}>
                    <rect
                      x={p.x - 16}
                      y={p.y - 12}
                      width="32"
                      height="24"
                      rx="3"
                      fill="var(--muted)"
                      stroke="currentColor"
                      strokeWidth="0.8"
                    />
                    <rect
                      x={p.x - 10}
                      y={p.y - 4}
                      width="20"
                      height="3"
                      fill="currentColor"
                      opacity="0.5"
                    />
                  </g>
                )),
              )}

              {/* user-tier nodes */}
              {users.map((u, i) => (
                <g key={`un-${i}`}>
                  <circle
                    cx={u.x}
                    cy={u.y}
                    r="20"
                    fill="var(--muted)"
                    stroke="currentColor"
                    strokeWidth="0.9"
                  />
                  <rect
                    x={u.x - 14}
                    y={u.y + 26}
                    width="28"
                    height="4"
                    fill="currentColor"
                    opacity="0.45"
                  />
                </g>
              ))}

              {/* central hub — larger, with a faint halo */}
              <circle
                cx={HUB.x}
                cy={HUB.y}
                r="38"
                fill="none"
                stroke="currentColor"
                strokeWidth="0.6"
                opacity="0.5"
              />
              <circle
                cx={HUB.x}
                cy={HUB.y}
                r="28"
                fill="var(--muted)"
                stroke="currentColor"
                strokeWidth="1.1"
              />
              <rect
                x={HUB.x - 22}
                y={HUB.y + 36}
                width="44"
                height="5"
                fill="currentColor"
                opacity="0.6"
              />
              <rect
                x={HUB.x - 16}
                y={HUB.y + 45}
                width="32"
                height="4"
                fill="currentColor"
                opacity="0.4"
              />
            </g>

            {/* HUD top-right — 4 stacked mono-line readouts. */}
            <g transform="translate(620, 24)" className="text-muted-foreground/60">
              {[0, 1, 2, 3].map((i) => (
                <rect
                  key={i}
                  x="0"
                  y={i * 12}
                  width={i === 3 ? 70 : 90}
                  height="4"
                  fill="currentColor"
                  opacity="0.5"
                />
              ))}
            </g>
          </svg>

          {/* Bottom-right pan/zoom toolbar — 4 stacked icon-button slots
              that match the real <LiveTopology> control rail exactly. */}
          <div className="absolute bottom-2 right-2 flex flex-col gap-1">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="size-7 rounded-sm" />
            ))}
          </div>
        </div>
      </div>
    </>
  )
}

/** /app/finder — Finder */
function FinderSkeleton() {
  return (
    <>
      <SkPageHead />
      <SkPanel>
        <div className="flex flex-col gap-4">
          <Skeleton className="h-8 w-full rounded-md" />
          <div className="flex items-center gap-2">
            <Skeleton className="h-7 w-24 rounded-md" />
            <Skeleton className="h-7 w-24 rounded-md" />
            <Skeleton className="h-7 w-16 rounded-md" />
          </div>
        </div>
      </SkPanel>
    </>
  )
}

/** /app/settings — Settings */
function SettingsSkeleton() {
  return (
    <>
      <SkPageHead />
      <div className="grid gap-6 lg:grid-cols-[220px_1fr]">
        <aside className="border-border lg:border">
          {Array.from({ length: 5 }).map((_, i) => (
            <div key={i} className="border-border flex items-center gap-2 border-b px-3 py-3 last:border-b-0">
              <Skeleton className="size-3 rounded-sm" />
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <Skeleton className="h-3 w-20 rounded-none" />
                <Skeleton className="h-2.5 w-28 rounded-none" />
              </div>
            </div>
          ))}
        </aside>
        <section className="flex flex-col gap-6">
          <div className="flex flex-col gap-2">
            <Skeleton className="h-2.5 w-16 rounded-none" />
            <Skeleton className="h-6 w-40 rounded-none" />
          </div>
          <SkPanel>
            <Skeleton className="h-24 w-full rounded-none" />
          </SkPanel>
          <SkPanel>
            <Skeleton className="h-24 w-full rounded-none" />
          </SkPanel>
        </section>
      </div>
    </>
  )
}

/** /app/security */
function SecuritySkeleton() {
  return (
    <>
      <SkPageHead />
      <SkPanel>
        <div className="flex flex-col gap-3">
          <div className="flex items-center gap-3">
            <Skeleton className="h-5 w-20 rounded-md" />
            <Skeleton className="h-3 w-64 rounded-none" />
          </div>
          <div className="flex gap-2">
            <Skeleton className="h-8 w-28 rounded-md" />
          </div>
        </div>
      </SkPanel>
      <SkPanel>
        <Skeleton className="h-8 w-40 rounded-md" />
      </SkPanel>
    </>
  )
}

/** /app/account */
function AccountSkeleton() {
  return (
    <>
      <SkPageHead />
      <SkPanel>
        <div className="flex flex-col gap-3">
          <div className="flex items-baseline justify-between">
            <Skeleton className="h-3 w-16 rounded-none" />
            <Skeleton className="h-3 w-48 rounded-none" />
          </div>
          <div className="flex items-baseline justify-between">
            <Skeleton className="h-3 w-16 rounded-none" />
            <Skeleton className="h-3 w-20 rounded-none" />
          </div>
        </div>
      </SkPanel>
      <SkPanel>
        <Skeleton className="h-12 w-full rounded-none" />
        <Skeleton className="mt-3 h-8 w-40 rounded-md" />
      </SkPanel>
    </>
  )
}

/** /admin — Admin Overview */
function AdminOverviewSkeleton() {
  return (
    <>
      <SkPageHead right="button" />
      <SkKpiStrip />
      <SkPanel>
        <Skeleton className="h-3 w-72 rounded-none" />
      </SkPanel>
      <SkPanel>
        <div className="grid gap-4 md:grid-cols-2">
          {Array.from({ length: 2 }).map((_, i) => (
            <div key={i} className="border-border bg-card rounded-md border p-4">
              <div className="flex items-baseline justify-between pb-3">
                <Skeleton className="h-3 w-32 rounded-none" />
                <Skeleton className="h-3 w-16 rounded-none" />
              </div>
              <Skeleton className="h-[120px] w-full rounded-none" />
            </div>
          ))}
        </div>
      </SkPanel>
      <SkPanel flush>
        <SkTableRows rows={5} cols={6} />
      </SkPanel>
    </>
  )
}

/** Generic admin table page (Users, Audit, Failed logins) */
function AdminTableSkeleton({ withKpis = false }: { withKpis?: boolean }) {
  return (
    <>
      <SkPageHead right="search" />
      {withKpis && <SkKpiStrip />}
      <SkPanel flush>
        <SkTableRows rows={8} cols={6} />
      </SkPanel>
    </>
  )
}

/** /admin/servers — Servers admin (stack of panels) */
function ServersSkeleton() {
  return (
    <>
      <SkPageHead />
      {Array.from({ length: 2 }).map((_, i) => (
        <SkPanel key={i}>
          <div className="flex flex-col gap-3">
            <Skeleton className="h-8 w-full rounded-md" />
            <div className="grid gap-3 sm:grid-cols-2">
              <Skeleton className="h-8 w-full rounded-md" />
              <Skeleton className="h-8 w-full rounded-md" />
            </div>
            <Skeleton className="h-8 w-full rounded-md" />
            <Skeleton className="h-8 w-full rounded-md" />
          </div>
        </SkPanel>
      ))}
    </>
  )
}

/** Fallback generic — used when no specific skeleton matches the route. */
function GenericRouteSkeleton() {
  return (
    <>
      <SkPageHead />
      <SkKpiStrip />
      <SkPanel>
        <Skeleton className="h-64 w-full rounded-none" />
      </SkPanel>
    </>
  )
}

// ── Switcher ──────────────────────────────────────────────────────────

/**
 * Picks the per-route skeleton. Order matters — more-specific routes
 * (e.g. `/app/devices/:id`) must be checked before their parent.
 */
export function RouteSkeleton({ pathname }: { pathname: string }) {
  if (pathname === "/app") return <DashboardSkeleton />
  if (pathname === "/app/devices") return <DevicesSkeleton />
  if (pathname.startsWith("/app/devices/")) return <DeviceDetailSkeleton />
  if (pathname === "/app/topology") return <TopologySkeleton />
  if (pathname === "/app/finder") return <FinderSkeleton />
  if (pathname === "/app/settings") return <SettingsSkeleton />
  if (pathname === "/app/security") return <SecuritySkeleton />
  if (pathname === "/app/account") return <AccountSkeleton />
  if (pathname === "/admin") return <AdminOverviewSkeleton />
  if (pathname === "/admin/users") return <AdminTableSkeleton />
  if (pathname === "/admin/audit") return <AdminTableSkeleton />
  if (pathname === "/admin/failed-logins") return <AdminTableSkeleton withKpis />
  if (pathname === "/admin/servers") return <ServersSkeleton />
  if (pathname === "/admin/online-devices") return <FinderSkeleton />
  return <GenericRouteSkeleton />
}
