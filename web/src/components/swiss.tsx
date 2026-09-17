/* eslint-disable react-refresh/only-export-components -- design-system
   module: components and their sibling format helpers (fmtRel, …) ship
   together by design; a full reload on edit is acceptable here. */
import * as React from "react"

import { cn } from "@/lib/utils"

/* ── Logomark / Wordmark ───────────────────────────────────────────────
   Crime Tracker VPN brand: a square 0-with-a-slash drawn from hairlines. The "0"
   in ZER0VPN tints to accent so the mark reads even at 12px. */

export function Logomark({
  size = 18,
  className,
}: {
  size?: number
  className?: string
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 20 20"
      fill="none"
      className={cn("shrink-0", className)}
      aria-hidden
    >
      <line
        x1="1"
        y1="10"
        x2="5.5"
        y2="10"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <line
        x1="14.5"
        y1="10"
        x2="19"
        y2="10"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <circle
        cx="10"
        cy="10"
        r="4.25"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <circle cx="10" cy="10" r="1.6" fill="var(--primary)" />
    </svg>
  )
}

/** Full-page boot loader. Uses the Logomark geometry but animates the
 *  primary-color core + an expanding halo via SMIL so it works without any
 *  JS animation lib and isn't gated by `prefers-reduced-motion` framework
 *  hooks. Drop in anywhere a Suspense fallback or auth-bootstrap blank
 *  screen would otherwise render. */
export function LogoLoader({
  size = 56,
  caption = "booting",
  className,
}: {
  size?: number
  caption?: string
  className?: string
}) {
  return (
    <div
      className={cn(
        "flex min-h-svh flex-col items-center justify-center gap-4 text-foreground",
        className
      )}
      role="status"
      aria-live="polite"
    >
      <svg
        width={size}
        height={size}
        viewBox="0 0 20 20"
        fill="none"
        className="shrink-0"
        aria-hidden
      >
        <line
          x1="1"
          y1="10"
          x2="5.5"
          y2="10"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
        <line
          x1="14.5"
          y1="10"
          x2="19"
          y2="10"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
        <circle
          cx="10"
          cy="10"
          r="4.25"
          stroke="currentColor"
          strokeWidth="1.5"
        />
        {/* expanding halo — fades as it grows past the ring */}
        <circle
          cx="10"
          cy="10"
          r="4.25"
          fill="none"
          stroke="var(--primary)"
          strokeWidth="1.25"
          opacity="0"
        >
          <animate
            attributeName="r"
            values="4.25;7;9"
            dur="1.8s"
            repeatCount="indefinite"
          />
          <animate
            attributeName="opacity"
            values="0.55;0.2;0"
            dur="1.8s"
            repeatCount="indefinite"
          />
        </circle>
        {/* pulsing core — the live handshake */}
        <circle cx="10" cy="10" r="1.6" fill="var(--primary)">
          <animate
            attributeName="r"
            values="1.6;2.4;1.6"
            dur="1.4s"
            repeatCount="indefinite"
          />
          <animate
            attributeName="opacity"
            values="1;0.5;1"
            dur="1.4s"
            repeatCount="indefinite"
          />
        </circle>
      </svg>
      {caption && (
        <span className="font-mono text-[10px] tracking-[0.18em] text-muted-foreground/70 uppercase">
          {caption}
          <span className="ml-0.5 inline-block animate-pulse text-primary">
            ▍
          </span>
        </span>
      )}
      <span className="sr-only">Loading</span>
    </div>
  )
}

export function Wordmark({
  size = 14,
  className,
}: {
  size?: number
  className?: string
}) {
  return (
    <span className={cn("inline-flex items-center gap-2", className)}>
      <Logomark size={size + 4} />
      <span
        className="font-mono font-medium tracking-[0.04em]"
        style={{ fontSize: size }}
      >
        ZER<span className="text-primary">0</span>VPN
      </span>
    </span>
  )
}

/* ── Eyebrow ────────────────────────────────────────────────────────────
   Small monospace label that anchors a page / section. Optional numeric
   prefix matches the design's "01" / "02" rhythm. */

export function Eyebrow({
  num,
  children,
  className,
}: {
  num?: React.ReactNode
  children: React.ReactNode
  className?: string
}) {
  return (
    <div className={cn("zv-eyebrow flex items-center gap-2", className)}>
      {num != null && <span className="opacity-60">{num}</span>}
      <span>{children}</span>
    </div>
  )
}

/* ── PageHead ──────────────────────────────────────────────────────────
   Title slab — eyebrow + h1 + sub + right-side actions, hairline divider. */

export function PageHead({
  eyebrow,
  title,
  sub,
  right,
  children,
}: {
  eyebrow?: React.ReactNode
  title: React.ReactNode
  sub?: React.ReactNode
  right?: React.ReactNode
  children?: React.ReactNode
}) {
  return (
    <div className="zv-page-head">
      <div className="flex min-w-0 flex-1 basis-[240px] flex-col gap-1">
        {eyebrow && <Eyebrow>{eyebrow}</Eyebrow>}
        <h1 className="font-heading break-words">{title}</h1>
        {sub && (
          <div className="mt-1 text-[13px] text-muted-foreground">{sub}</div>
        )}
        {children}
      </div>
      {right && (
        <div className="flex flex-wrap items-center justify-end gap-2 max-sm:w-full">
          {right}
        </div>
      )}
    </div>
  )
}

/* ── Panel ─────────────────────────────────────────────────────────────
   The bread-and-butter Swiss card: hairline border, optional head with
   title/sub/actions, body padding togglable via `flush`. */

export function Panel({
  title,
  sub,
  right,
  footer,
  flush,
  className,
  bodyClassName,
  children,
}: {
  title?: React.ReactNode
  sub?: React.ReactNode
  right?: React.ReactNode
  footer?: React.ReactNode
  flush?: boolean
  className?: string
  bodyClassName?: string
  children: React.ReactNode
}) {
  return (
    <div className={cn("zv-panel", className)}>
      {(title || right) && (
        <div className="zv-panel-head">
          <div className="flex min-w-0 flex-col gap-0.5">
            {title && <h3>{title}</h3>}
            {sub && <div className="zv-panel-sub truncate">{sub}</div>}
          </div>
          {right && (
            <div className="flex flex-wrap items-center justify-end gap-2 max-sm:w-full">
              {right}
            </div>
          )}
        </div>
      )}
      <div className={cn("zv-panel-body", flush && "flush", bodyClassName)}>
        {children}
      </div>
      {footer && (
        <div className="zv-panel-head border-t border-b-0 border-border">
          {footer}
        </div>
      )}
    </div>
  )
}

/* ── Pill ──────────────────────────────────────────────────────────────
   Outlined status capsule — `tone` selects color via CSS data-attr. */

export type PillTone = "ok" | "warn" | "err" | "info" | "paused" | "neutral"

export function Pill({
  tone = "neutral",
  dot = true,
  children,
  className,
}: {
  tone?: PillTone
  dot?: boolean
  children: React.ReactNode
  className?: string
}) {
  return (
    <span
      className={cn("zv-pill", className)}
      data-tone={tone === "neutral" ? undefined : tone}
    >
      {dot && <span className="zv-pill-dot" />}
      <span>{children}</span>
    </span>
  )
}

/* ── Sparkline ─────────────────────────────────────────────────────────
   Tiny inline chart for KPIs. SVG-only, vector-effect non-scaling so the
   stroke stays 1.2-1.4px regardless of container width. */

function Sparkline({
  data,
  height = 26,
  kind = "area",
  color = "var(--primary)",
}: {
  data: readonly number[]
  height?: number
  kind?: "area" | "line" | "bar"
  color?: string
}) {
  const w = 100
  const h = height
  if (data.length === 0) return <svg width={w} height={h} />
  const max = Math.max(...data, 1)
  const min = Math.min(...data, 0)
  const range = max - min || 1
  const pts = data.map((v, i) => {
    const x = (i / Math.max(1, data.length - 1)) * w
    const y = h - ((v - min) / range) * (h - 2) - 1
    return [x, y] as const
  })
  const linePath = pts
    .map(([x, y], i) => (i === 0 ? `M${x},${y}` : `L${x},${y}`))
    .join(" ")

  if (kind === "bar") {
    const bw = (w / data.length) * 0.8
    return (
      <svg
        viewBox={`0 0 ${w} ${h}`}
        preserveAspectRatio="none"
        style={{ width: "100%", height: h }}
      >
        {data.map((v, i) => {
          const bh = ((v - min) / range) * (h - 2)
          const x = (i / data.length) * w
          const y = h - bh - 1
          return (
            <rect
              key={i}
              x={x}
              y={y}
              width={bw}
              height={bh}
              fill={color}
              opacity="0.9"
            />
          )
        })}
      </svg>
    )
  }
  if (kind === "line") {
    return (
      <svg
        viewBox={`0 0 ${w} ${h}`}
        preserveAspectRatio="none"
        style={{ width: "100%", height: h }}
      >
        <path
          d={linePath}
          stroke={color}
          fill="none"
          strokeWidth="1.4"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
    )
  }
  const areaPath = `${linePath} L${w},${h} L0,${h} Z`
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      preserveAspectRatio="none"
      style={{ width: "100%", height: h }}
    >
      <path d={areaPath} fill={color} opacity="0.18" />
      <path
        d={linePath}
        stroke={color}
        fill="none"
        strokeWidth="1.2"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  )
}

/* ── KPI strip + KPI ───────────────────────────────────────────────────
   Four-up labelled blocks. Use <Kpi> as the children of <KpiStrip>. */

export function KpiStrip({
  children,
  className,
  cols = 4,
}: {
  children: React.ReactNode
  className?: string
  /** Columns on wide screens. Defaults to 4; pass 5 for the dashboard's
   *  Devices/RX/TX/Usage/Quota strip. The CSS variant keeps the responsive
   *  fallbacks (`.zv-kpi-strip[data-cols="5"]` in index.css). */
  cols?: 4 | 5
}) {
  return (
    <div className={cn("zv-kpi-strip", className)} data-cols={cols}>
      {children}
    </div>
  )
}

export function Kpi({
  label,
  value,
  unit,
  spark,
  sparkKind = "area",
  sparkColor,
  footL,
  footR,
  deltaTone,
}: {
  label: React.ReactNode
  value: React.ReactNode
  unit?: React.ReactNode
  spark?: readonly number[]
  sparkKind?: "area" | "line" | "bar"
  sparkColor?: string
  footL?: React.ReactNode
  footR?: React.ReactNode
  deltaTone?: "up" | "dn"
}) {
  return (
    <div className="zv-kpi">
      <div className="zv-kpi-label">
        <span>{label}</span>
      </div>
      <div className="zv-kpi-val font-heading">
        <span>{value}</span>
        {unit != null && <sup>{unit}</sup>}
      </div>
      {spark && spark.length > 0 && (
        <div className="h-[26px]">
          <Sparkline data={spark} kind={sparkKind} color={sparkColor} />
        </div>
      )}
      {(footL || footR) && (
        <div className="zv-kpi-foot">
          <span
            className={
              deltaTone === "up"
                ? "zv-delta-up"
                : deltaTone === "dn"
                  ? "zv-delta-dn"
                  : undefined
            }
          >
            {footL}
          </span>
          {footR && <span>{footR}</span>}
        </div>
      )}
    </div>
  )
}

/* ── CodeBlock ─────────────────────────────────────────────────────────
   Pre-formatted monospace block — wg-conf, curl examples, docker compose. */

export function CodeBlock({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return <pre className={cn("zv-code", className)}>{children}</pre>
}

/* ── Segmented control ────────────────────────────────────────────────
   Uppercase mono segment selector. Generic over option value. */

export function Seg<T extends string>({
  value,
  options,
  onChange,
  className,
}: {
  value: T
  options: readonly (T | { value: T; label: React.ReactNode })[]
  onChange: (v: T) => void
  className?: string
}) {
  return (
    <div className={cn("zv-seg", className)}>
      {options.map((o) => {
        const v = (typeof o === "object" ? o.value : o) as T
        const l = typeof o === "object" ? o.label : o
        return (
          <button
            key={v}
            type="button"
            data-active={v === value ? "1" : "0"}
            onClick={() => onChange(v)}
          >
            {l}
          </button>
        )
      })}
    </div>
  )
}

/* ── LiveDot ───────────────────────────────────────────────────────────
   Pulsing green status indicator — connected / streaming. */

export function LiveDot({
  state = "live",
  className,
}: {
  state?: "live" | "offline" | "warn"
  className?: string
}) {
  return (
    <span
      className={cn("zv-live-dot", className)}
      data-state={state === "live" ? undefined : state}
      aria-hidden
    />
  )
}

/* ── Kbd ──────────────────────────────────────────────────────────────
   Inline mono keycap. Use for ⌘K, ↵, etc. */

export function Kbd({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return <span className={cn("zv-kbd", className)}>{children}</span>
}

/* ── Format helpers ────────────────────────────────────────────────────
   Mirror the prototype's helpers; pages import these instead of inlining. */

export function fmtRel(ms: number): string {
  if (ms < 60_000) return Math.floor(ms / 1000) + "s ago"
  if (ms < 3_600_000) return Math.floor(ms / 60_000) + "m ago"
  if (ms < 86_400_000) return Math.floor(ms / 3_600_000) + "h ago"
  return Math.floor(ms / 86_400_000) + "d ago"
}
