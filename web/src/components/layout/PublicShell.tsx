import { motion } from "motion/react"
import { Suspense } from "react"
import { Outlet, useLocation } from "react-router"

import { AuthSkeleton } from "@/components/layout/AuthShell"
import { Skeleton } from "@/components/ui/skeleton"
import { TooltipProvider } from "@/components/ui/tooltip"
import { EASING, TIMING, useReducedMotion } from "@/lib/motion"

/** Swiss public shell. Flat paper background — auth pages supply their
 * own grid texture via the .zv-grid-bg utility where they want it (the
 * auth side panel). No global radial halo.
 *
 * Route transitions use a plain keyed mount animation rather than
 * `AnimatePresence mode="wait"` — the wait-mode + Suspense combo can
 * leave the new page stuck at initial opacity when a lazy chunk
 * resolves mid-flight. Simpler is more reliable.
 *
 * Suspense fallback is path-aware so /login gets a skeleton
 * that matches their actual layout (no jump when the chunk resolves).
 * Other public paths fall back to a small generic skeleton. */
export function PublicShell() {
  const location = useLocation()

  return (
    <TooltipProvider delayDuration={250}>
      <div className="bg-background text-foreground relative min-h-svh">
        <PageMount key={location.pathname}>
          <Suspense fallback={<PublicFallback pathname={location.pathname} />}>
            <Outlet />
          </Suspense>
        </PageMount>
      </div>
    </TooltipProvider>
  )
}

function PublicFallback({ pathname }: { pathname: string }) {
  if (pathname === "/login") return <AuthSkeleton />
  // Everything else — small centered skeleton.
  return (
    <div className="flex min-h-svh items-center justify-center p-6">
      <Skeleton className="h-64 w-full max-w-sm rounded-none" />
    </div>
  )
}

function PageMount({ children }: { children: React.ReactNode }) {
  const reduce = useReducedMotion()
  if (reduce) return <div className="relative">{children}</div>
  // Matches the dashboard's PageMount feel — same timing, easing, and
  // entry-delay so /login → /app reads as a single coherent
  // transition style across both shells.
  return (
    <motion.div
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{
        duration: TIMING.enter,
        ease: EASING.out,
        delay: TIMING.routeDelay,
      }}
      className="relative"
    >
      {children}
    </motion.div>
  )
}
