import type { ReactNode } from "react"

import { Eyebrow, LiveDot, Wordmark } from "@/components/swiss"
import { Skeleton } from "@/components/ui/skeleton"

/**
 * Two-column public auth chrome. Left side: brand + marketing copy + a
 * status strip; collapses on mobile. Right side: the form (caller's
 * `children`). Matches auth.jsx in the design bundle. Pages compose
 * `<AuthShell><AuthFormCard>…</AuthFormCard></AuthShell>` so the form
 * itself can vary while the surrounding chrome stays consistent.
 */
export function AuthShell({
  left,
  children,
}: {
  /** Override the default left-side promo block. Optional. */
  left?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="grid min-h-svh grid-cols-1 md:grid-cols-2">
      <aside className="zv-grid-bg bg-muted/40 relative hidden flex-col justify-between gap-12 overflow-hidden border-r p-8 md:flex">
        <div className="flex items-center gap-2">
          <Wordmark size={14} />
        </div>
        {left || <DefaultPromo />}
        <div className="text-muted-foreground flex flex-wrap items-center gap-3 font-mono text-[11px]">
          <span>v1.0.20240310</span>
          <span>·</span>
          <span>region · self-hosted</span>
          <span>·</span>
          <span className="inline-flex items-center gap-1.5">
            <LiveDot />
            api ok
          </span>
        </div>
      </aside>
      <div className="flex items-center justify-center p-8">{children}</div>
    </div>
  )
}

function DefaultPromo() {
  return (
    <div className="flex max-w-[380px] flex-col gap-6">
      <Eyebrow num="—">Self-hosted · No-logs</Eyebrow>
      <h2 className="font-heading m-0 text-3xl font-medium leading-[1.05] tracking-[-0.01em]">
        A VPN console you
        <br />
        actually trust —
        <br />
        because you run it.
      </h2>
      <p className="text-muted-foreground m-0 text-sm leading-relaxed">
        Every operation, every login, every byte counted is on your machine.
        Argon2 hashes, KEK-encrypted secrets, no third-party in the path.
      </p>
    </div>
  )
}

/** The form container used by every auth page. 360px wide, gap-5 stack. */
export function AuthForm({
  children,
  onSubmit,
}: {
  children: ReactNode
  onSubmit?: React.FormEventHandler<HTMLFormElement>
}) {
  return (
    <form className="flex w-[360px] max-w-full flex-col gap-5" onSubmit={onSubmit}>
      {children}
    </form>
  )
}

/** The headline block at the top of each auth page (eyebrow + h1). */
export function AuthHeading({
  eyebrow,
  children,
}: {
  eyebrow: ReactNode
  children: ReactNode
}) {
  return (
    <div className="flex flex-col gap-1">
      <Eyebrow>{eyebrow}</Eyebrow>
      <h1 className="font-heading m-0 text-[28px] font-medium tracking-[-0.01em]">
        {children}
      </h1>
    </div>
  )
}

/** Mono uppercase footer rule + tiny copy. Sits below every auth form. */
export function AuthFooterRule({
  children,
}: {
  children?: ReactNode
}) {
  return (
    <>
      <div className="zv-hrule mt-6" />
      <div className="text-muted-foreground flex items-center justify-between font-mono text-[10px] uppercase tracking-[0.06em]">
        <span>secured · argon2 · totp</span>
        {children}
      </div>
    </>
  )
}

/**
 * Suspense fallback for `/login`. Mirrors the AuthShell
 * + AuthForm layout so the page doesn't shift when the lazy chunk
 * resolves — same 2-column grid, same 360px form well, same
 * heading / field / button / footer-rule rhythm.
 *
 * Pass `inputs=0` for the Google-only login shape.
 */
export function AuthSkeleton({ inputs = 0 }: { inputs?: number }) {
  return (
    <div className="grid min-h-svh grid-cols-1 md:grid-cols-2">
      {/* Left aside — matches AuthShell's promo column exactly so the
          right form well doesn't jump when the page mounts. */}
      <aside className="zv-grid-bg bg-muted/40 relative hidden flex-col justify-between gap-12 overflow-hidden border-r p-8 md:flex">
        <div className="flex items-center gap-2">
          <Wordmark size={14} />
        </div>
        <div className="flex max-w-[380px] flex-col gap-6">
          <Skeleton className="h-3 w-24 rounded-none" />
          <div className="flex flex-col gap-2">
            <Skeleton className="h-9 w-[80%] rounded-none" />
            <Skeleton className="h-9 w-[72%] rounded-none" />
            <Skeleton className="h-9 w-[60%] rounded-none" />
          </div>
          <div className="flex flex-col gap-2">
            <Skeleton className="h-3 w-full rounded-none" />
            <Skeleton className="h-3 w-[90%] rounded-none" />
            <Skeleton className="h-3 w-[55%] rounded-none" />
          </div>
        </div>
        <div className="text-muted-foreground flex flex-wrap items-center gap-3 font-mono text-[11px]">
          <span className="inline-flex items-center gap-1.5">
            <LiveDot state="warn" />
            loading
          </span>
        </div>
      </aside>

      <div className="flex items-center justify-center p-8">
        <div className="flex w-[360px] max-w-full flex-col gap-5">
          {/* AuthHeading — eyebrow (~80px) + h1 (text-[28px] ≈ 36px tall) */}
          <div className="flex flex-col gap-2">
            <Skeleton className="h-3 w-20 rounded-none" />
            <Skeleton className="h-9 w-[78%] rounded-none" />
          </div>

          {/* Field rows — eyebrow label (~64px wide, ~10px tall) + input
              (h-8 to match the Input component exactly). */}
          {Array.from({ length: inputs }).map((_, i) => (
            <div key={i} className="flex flex-col gap-1.5">
              <Skeleton className="h-2.5 w-16 rounded-none" />
              <Skeleton className="h-8 w-full rounded-md" />
            </div>
          ))}

          {/* Primary CTA — h-9 matches the Button size="lg" exactly. */}
          <Skeleton className="h-9 w-full rounded-md" />

          {/* "New here? / Sign in ↗" footer link row. */}
          <div className="flex items-center justify-between">
            <Skeleton className="h-3 w-20 rounded-none" />
            <Skeleton className="h-3 w-28 rounded-none" />
          </div>

          {/* AuthFooterRule — hrule + small mono caption row. */}
          <div className="mt-6 flex flex-col gap-2">
            <div className="zv-hrule" />
            <div className="flex items-center justify-between">
              <Skeleton className="h-2.5 w-32 rounded-none" />
              <Skeleton className="h-2.5 w-12 rounded-none" />
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
