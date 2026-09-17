import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconBell,
  IconCheck,
  IconPalette,
  IconSettings,
  IconShield,
  IconUser,
  type Icon,
} from "@tabler/icons-react"
import { AnimatePresence, motion } from "motion/react"
import { useLocation, useNavigate } from "react-router"
import { toast } from "sonner"

import { PageStagger, StaggerItem } from "@/components/motion"
import { formatDateWith, formatTimeWith } from "@/lib/datetime"
import { EASING, TIMING, useReducedMotion } from "@/lib/motion"
import { formatBpsWith } from "@/lib/units"
import {
  useTheme,
  type Accent,
  type ThemeVariant,
} from "@/components/theme-provider"
import { Eyebrow, PageHead, Panel, Seg } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Skeleton } from "@/components/ui/skeleton"
import {
  ApiError,
  getMyPreferences,
  setMyPreferences,
  type DateFormatPref,
  type DefaultLandingPref,
  type TimeFormatPref,
  type ToastPositionPref,
  type UnitsPref,
  type UserPreferences,
} from "@/lib/api"
import { notify, previewChime } from "@/lib/notify"
import { AccountSections } from "@/pages/app/Account"
import { SecuritySections } from "@/pages/app/Security"

// ── Sub-nav model ─────────────────────────────────────────────────────
type SectionKey =
  | "appearance"
  | "preferences"
  | "notifications"
  | "account"
  | "security"

interface SectionDef {
  key: SectionKey
  label: string
  hash: string
  icon: Icon
  hint: string
}

const SECTIONS: SectionDef[] = [
  {
    key: "account",
    label: "Account",
    hash: "account",
    icon: IconUser,
    hint: "Profile, data, lifecycle",
  },
  {
    key: "security",
    label: "Security",
    hash: "security",
    icon: IconShield,
    hint: "2FA, sessions, recovery codes",
  },
  {
    key: "appearance",
    label: "Appearance",
    hash: "appearance",
    icon: IconPalette,
    hint: "Theme, accent color",
  },
  {
    key: "preferences",
    label: "Preferences",
    hash: "preferences",
    icon: IconSettings,
    hint: "Units, dates, defaults",
  },
  {
    key: "notifications",
    label: "Notifications",
    hash: "notifications",
    icon: IconBell,
    hint: "Toasts, browser alerts",
  },
]

const HASH_TO_KEY: Record<string, SectionKey> = Object.fromEntries(
  SECTIONS.map((s) => [s.hash, s.key])
) as Record<string, SectionKey>

const ACCENT_SWATCHES: { value: Accent; label: string; preview: string }[] = [
  { value: "lime", label: "Lime", preview: "#C6FF3D" },
  { value: "cobalt", label: "Cobalt", preview: "#3D5BFF" },
  { value: "orange", label: "Orange", preview: "#FF6A1F" },
  { value: "magenta", label: "Magenta", preview: "#FF2E88" },
  { value: "ink", label: "Ink", preview: "#0A0A0A" },
]

// RX/TX chart-line color options. "accent" follows the accent (--primary);
// the rest are fixed hues. The active swatch gets the same accent highlight
// (zv-accent-swatch--on) as the accent picker.
const CHART_PALETTE: { value: string; label: string; preview: string }[] = [
  { value: "accent", label: "Accent", preview: "var(--primary)" },
  { value: "#3D5BFF", label: "Cobalt", preview: "#3D5BFF" },
  { value: "#06B6D4", label: "Cyan", preview: "#06B6D4" },
  { value: "#1F8A3F", label: "Green", preview: "#1F8A3F" },
  { value: "#C6FF3D", label: "Lime", preview: "#C6FF3D" },
  { value: "#FF6A1F", label: "Orange", preview: "#FF6A1F" },
  { value: "#FF2E88", label: "Magenta", preview: "#FF2E88" },
  { value: "#A855F7", label: "Purple", preview: "#A855F7" },
  { value: "#C5283D", label: "Red", preview: "#C5283D" },
]

export function SettingsPage() {
  const location = useLocation()
  const navigate = useNavigate()
  const reduce = useReducedMotion()
  // Active section derives straight from the URL hash, so deep links (e.g.
  // /app/settings#appearance) and back/forward navigation work with no
  // state syncing — selecting a section just navigates.
  const active: SectionKey =
    HASH_TO_KEY[location.hash.replace(/^#/, "")] ?? "account"

  const selectSection = (key: SectionKey) => {
    const def = SECTIONS.find((s) => s.key === key)
    if (!def) return
    navigate(`/app/settings#${def.hash}`, { replace: false })
  }

  const activeDef = SECTIONS.find((s) => s.key === active) ?? SECTIONS[0]

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Workspace · 07"
          title="Settings"
          sub="Profile, appearance, and account-wide preferences."
        />
      </StaggerItem>

      <StaggerItem className="grid gap-6 lg:grid-cols-[220px_1fr]">
        {/* Sub-nav — vertical on desktop. Each row is the section label
            + a one-liner hint so the page is self-documenting. */}
        <aside className="border-border lg:sticky lg:top-6 lg:self-start lg:border">
          <ul className="divide-border lg:divide-y">
            {SECTIONS.map((s) => {
              const isActive = s.key === active
              const I = s.icon
              return (
                <li key={s.key} className="relative">
                  {/* Active-row indicator. Sharing `layoutId` across the
                      five list items means Framer Motion animates this
                      element from the previously-active item's box to
                      the newly-active one — a single highlight that
                      slides, not five highlights that fade in and out.
                      Held to the same easing curve the route shells use
                      so it feels like one transition system. */}
                  {isActive && (
                    <motion.div
                      layoutId="settings-nav-active"
                      className="absolute inset-0 z-0 bg-primary/8"
                      transition={
                        reduce
                          ? { duration: 0 }
                          : { duration: TIMING.enter, ease: EASING.out }
                      }
                      aria-hidden
                    >
                      <span className="absolute inset-y-0 left-0 w-[2px] bg-primary" />
                    </motion.div>
                  )}
                  <button
                    type="button"
                    onClick={() => selectSection(s.key)}
                    data-active={isActive ? "1" : undefined}
                    className="zv-settings-nav-item relative z-10 w-full"
                  >
                    <I size={14} className="shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 text-left">
                      <span className="block text-[13px] font-medium text-foreground">
                        {s.label}
                      </span>
                      <span className="block font-mono text-[10px] text-muted-foreground/80">
                        {s.hint}
                      </span>
                    </span>
                  </button>
                </li>
              )
            })}
          </ul>
        </aside>

        <section className="flex min-w-0 flex-col gap-6">
          {/* Title + body live inside the same motion.div so they
              transition as one unit when the active section changes.
              Splitting them — title outside, body inside AnimatePresence
              — made the title swap instantly while the old body was
              still exiting, which read as a "shutter" / mismatch when
              clicking tabs in quick succession.
              `mode="wait"` is safe here because every section is
              statically imported (no Suspense boundary inside this
              view). `initial={false}` keeps the first paint quiet so
              the parent StaggerItem owns the page's entry animation. */}
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={active}
              className="flex flex-col gap-6"
              initial={reduce ? false : { opacity: 0, y: 8 }}
              animate={
                reduce
                  ? undefined
                  : {
                      opacity: 1,
                      y: 0,
                      transition: {
                        duration: TIMING.enter,
                        ease: EASING.out,
                        delay: TIMING.routeDelay,
                      },
                    }
              }
              exit={
                reduce
                  ? undefined
                  : {
                      opacity: 0,
                      y: -6,
                      transition: {
                        duration: TIMING.exit,
                        ease: EASING.in,
                      },
                    }
              }
            >
              <div>
                <Eyebrow>{activeDef.label}</Eyebrow>
                <h2 className="mt-1 font-heading text-xl font-medium tracking-[-0.01em] text-foreground">
                  {activeDef.hint}
                </h2>
              </div>
              {active === "appearance" && <AppearanceSection />}
              {active === "preferences" && <PreferencesSection />}
              {active === "notifications" && <NotificationsSection />}
              {active === "account" && <AccountSections />}
              {active === "security" && <SecuritySections />}
            </motion.div>
          </AnimatePresence>
        </section>
      </StaggerItem>
    </PageStagger>
  )
}

// ── Appearance ────────────────────────────────────────────────────────

// Shared swatch grid for the RX/TX chart-line color pickers. Mirrors the
// accent picker so the active selection reads the same way (accent highlight).
function ChartColorPicker({
  value,
  onChange,
}: {
  value: string
  onChange: (v: string) => void
}) {
  // Live preview of the actual line color (resolves "accent" → the accent).
  const previewColor = value === "accent" ? "var(--primary)" : value
  return (
    <div className="flex flex-col gap-3">
      {/* Sample showing how the line renders in charts. */}
      <div className="flex h-9 items-center border border-border/60 bg-muted/15 px-3">
        <span
          className="h-[3px] w-full rounded-full"
          style={{ background: previewColor }}
          aria-hidden
        />
      </div>
      <div className="flex flex-wrap gap-2">
        {CHART_PALETTE.map((opt) => {
          const isActive = opt.value === value
          return (
            <button
              key={opt.value}
              type="button"
              onClick={() => onChange(opt.value)}
              className={[
                "zv-accent-swatch",
                isActive && "zv-accent-swatch--on",
              ]
                .filter(Boolean)
                .join(" ")}
              aria-pressed={isActive}
              aria-label={opt.label}
              title={opt.label}
            >
              <span
                className="zv-accent-swatch__chip"
                style={{ background: opt.preview }}
                aria-hidden
              >
                {isActive && <IconCheck size={12} strokeWidth={3} />}
              </span>
              <span className="font-mono text-[11px] text-foreground/90">
                {opt.label}
              </span>
            </button>
          )
        })}
      </div>
    </div>
  )
}

// Visual theme variants surfaced by the picker. Each tile shows a mini
// preview of the theme's canvas + primary + a couple of chrome bars so
// the user can pick by look before committing. `previewBg / previewInk /
// previewAccent` are *literal* colors (not CSS vars), since each tile
// has to render in its own swatch independent of the currently-active
// theme. Keep these in sync with the theme blocks in index.css.
const THEME_VARIANTS: Array<{
  value: ThemeVariant
  label: string
  blurb: string
  previewBg: string
  previewInk: string
  previewAccent: string
  font: string
  radius: number
}> = [
  {
    value: "swiss",
    label: "Swiss",
    blurb: "Sharp · paper + ink · lime",
    previewBg: "#fafaf7",
    previewInk: "#0a0a0a",
    previewAccent: "#c6ff3d",
    font: "'Geist Variable', sans-serif",
    radius: 2,
  },
  {
    value: "brutalist",
    label: "Brutalist",
    blurb: "Heavy borders · 0px · mono",
    previewBg: "#efece4",
    previewInk: "#0a0a0a",
    previewAccent: "#ff3d00",
    font: "'JetBrains Mono Variable', monospace",
    radius: 0,
  },
  {
    value: "terminal",
    label: "Terminal",
    blurb: "Phosphor green · CRT vibe",
    previewBg: "#000000",
    previewInk: "#39ff14",
    previewAccent: "#39ff14",
    font: "'JetBrains Mono Variable', monospace",
    radius: 0,
  },
  {
    value: "editorial",
    label: "Editorial",
    blurb: "Serif headings · warm cream",
    previewBg: "#faf6ef",
    previewInk: "#1a1714",
    previewAccent: "#8a3324",
    font: "'Fraunces Variable', serif",
    radius: 10,
  },
  {
    value: "soft",
    label: "Soft",
    blurb: "Lavender · rounded 16px · airy",
    previewBg: "#f5f3ff",
    previewInk: "#2a2438",
    previewAccent: "#7c3aed",
    font: "'Plus Jakarta Sans Variable', sans-serif",
    radius: 16,
  },
]

function ThemeVariantPicker({
  value,
  onChange,
}: {
  value: ThemeVariant
  onChange: (next: ThemeVariant) => void
}) {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
      {THEME_VARIANTS.map((t) => {
        const isActive = t.value === value
        return (
          <button
            key={t.value}
            type="button"
            onClick={() => onChange(t.value)}
            className={[
              "group relative flex flex-col gap-2 border p-3 text-left transition-colors",
              isActive
                ? "border-primary ring-2 ring-primary"
                : "border-border hover:border-foreground/40",
            ].join(" ")}
            aria-pressed={isActive}
            aria-label={`Switch to ${t.label} theme`}
            style={{ borderRadius: "var(--radius)" }}
          >
            {/* Mini preview pane — paints with the theme's literal colors
                so each tile reads the same regardless of the currently-
                active theme. */}
            <div
              className="flex h-20 flex-col justify-between overflow-hidden p-2"
              style={{
                background: t.previewBg,
                color: t.previewInk,
                borderRadius: t.radius,
                border: `1px solid ${t.previewInk}22`,
                fontFamily: t.font,
              }}
            >
              <div className="flex items-center justify-between text-[10px] tracking-[0.08em] uppercase opacity-70">
                <span>{t.label}</span>
                <span
                  className="block size-2"
                  style={{
                    background: t.previewAccent,
                    borderRadius: t.radius > 0 ? "50%" : 0,
                  }}
                  aria-hidden
                />
              </div>
              <div className="flex flex-col gap-1">
                <span
                  className="block h-1"
                  style={{
                    background: t.previewAccent,
                    width: "60%",
                    borderRadius: t.radius,
                  }}
                  aria-hidden
                />
                <span
                  className="block h-1 opacity-30"
                  style={{
                    background: t.previewInk,
                    width: "85%",
                    borderRadius: t.radius,
                  }}
                  aria-hidden
                />
                <span
                  className="block h-1 opacity-20"
                  style={{
                    background: t.previewInk,
                    width: "45%",
                    borderRadius: t.radius,
                  }}
                  aria-hidden
                />
              </div>
            </div>
            <div className="flex items-baseline justify-between gap-2">
              <span className="font-mono text-xs text-foreground">
                {t.label}
              </span>
              {isActive && (
                <IconCheck size={12} className="text-primary" strokeWidth={3} />
              )}
            </div>
            <span className="font-mono text-[10px] leading-tight text-muted-foreground">
              {t.blurb}
            </span>
          </button>
        )
      })}
    </div>
  )
}

function AppearanceSection() {
  const qc = useQueryClient()
  const {
    theme,
    setTheme,
    accent,
    setAccent,
    variant,
    setVariant,
    rxColor,
    setRxColor,
    txColor,
    setTxColor,
  } = useTheme()
  // Mutation persists the variant to /me/preferences so the choice
  // follows the user across devices. Local `setVariant` already updates
  // <html data-theme> + localStorage instantly; this just adds the
  // server-sync side. On success we patch the cached prefs row so the
  // PreferencesSection (and the Toaster's variant applier in sonner.tsx)
  // see the new value without a refetch.
  const variantM = useMutation({
    mutationFn: (next: ThemeVariant) => setMyPreferences({ theme: next }),
    onSuccess: (data) => {
      qc.setQueryData(["me", "preferences"], data)
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  const pickVariant = (next: ThemeVariant) => {
    if (next === variant) return
    setVariant(next)
    variantM.mutate(next)
  }
  return (
    <div className="flex flex-col gap-6">
      <Panel
        title="Theme variant"
        sub="Visual language · colors, fonts, radius, spacing"
      >
        <ThemeVariantPicker value={variant} onChange={pickVariant} />
        <p className="mt-3 font-mono text-[11px] text-muted-foreground">
          Saves to your account so the choice follows you across devices.
          Light/dark mode below stays orthogonal — each variant ships both.
        </p>
      </Panel>

      <Panel title="Theme" sub="Light, dark, or follow the OS">
        <Seg
          value={theme}
          options={[
            { value: "light", label: "Light" },
            { value: "dark", label: "Dark" },
            { value: "system", label: "System" },
          ]}
          onChange={setTheme}
        />
        <p className="mt-3 font-mono text-[11px] text-muted-foreground">
          Press <span className="zv-kbd">D</span> anywhere outside an input to
          toggle light/dark instantly.
        </p>
      </Panel>

      <Panel
        title="Accent color"
        sub="Tints buttons, links, focus rings, and the live-throughput series"
      >
        <div className="flex flex-wrap gap-2">
          {ACCENT_SWATCHES.map((opt) => {
            const isActive = opt.value === accent
            return (
              <button
                key={opt.value}
                type="button"
                onClick={() => setAccent(opt.value)}
                className={[
                  "zv-accent-swatch",
                  isActive && "zv-accent-swatch--on",
                ]
                  .filter(Boolean)
                  .join(" ")}
                aria-pressed={isActive}
                aria-label={opt.label}
                title={opt.label}
              >
                <span
                  className="zv-accent-swatch__chip"
                  style={{ background: opt.preview }}
                  aria-hidden
                >
                  {isActive && <IconCheck size={12} strokeWidth={3} />}
                </span>
                <span className="font-mono text-[11px] text-foreground/90">
                  {opt.label}
                </span>
              </button>
            )
          })}
        </div>
        <p className="mt-3 font-mono text-[11px] text-muted-foreground">
          Stored locally per browser. Switching takes effect immediately without
          reloading.
        </p>
      </Panel>

      <Panel
        title="RX line color"
        sub="Download / received series across every traffic chart"
      >
        <ChartColorPicker value={rxColor} onChange={setRxColor} />
      </Panel>

      <Panel
        title="TX line color"
        sub="Upload / transmitted series across every traffic chart"
      >
        <ChartColorPicker value={txColor} onChange={setTxColor} />
      </Panel>
    </div>
  )
}

// ── Preferences ───────────────────────────────────────────────────────

function PreferencesSection() {
  const qc = useQueryClient()
  const prefsQ = useQuery({
    queryKey: ["me", "preferences"],
    queryFn: getMyPreferences,
  })
  const m = useMutation({
    mutationFn: (patch: Partial<UserPreferences>) => setMyPreferences(patch),
    onSuccess: (data) => {
      qc.setQueryData(["me", "preferences"], data)
      toast.success("Preferences saved")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  if (prefsQ.isLoading || !prefsQ.data) {
    return (
      <div className="flex flex-col gap-4">
        <Skeleton className="h-32 rounded-none" />
        <Skeleton className="h-32 rounded-none" />
      </div>
    )
  }
  const p = prefsQ.data
  const now = new Date()

  return (
    <div className="flex flex-col gap-6">
      <Panel title="Display" sub="How values are formatted across the app">
        <div className="flex flex-col gap-4">
          <PrefRow label="Throughput units" hint="Bandwidth and live rates">
            <div className="flex flex-col items-start gap-1.5 sm:items-end">
              <Seg<UnitsPref>
                value={p.units}
                options={[
                  { value: "bps", label: "bits/s" },
                  { value: "Bps", label: "bytes/s" },
                ]}
                onChange={(v) => m.mutate({ units: v })}
              />
              <span className="font-mono text-[11px] text-muted-foreground">
                e.g. {formatBpsWith(12_000_000, p.units)}
              </span>
            </div>
          </PrefRow>
          <PrefRow label="Date format" hint="Tables and exports">
            <div className="flex flex-col items-start gap-1.5 sm:items-end">
              <Seg<DateFormatPref>
                value={p.date_format}
                options={[
                  { value: "iso", label: "ISO" },
                  { value: "us", label: "US" },
                  { value: "eu", label: "EU" },
                ]}
                onChange={(v) => m.mutate({ date_format: v })}
              />
              <span className="font-mono text-[11px] text-muted-foreground">
                e.g. {formatDateWith(now, p.date_format)}
              </span>
            </div>
          </PrefRow>
          <PrefRow label="Time format" hint="Timestamps">
            <div className="flex flex-col items-start gap-1.5 sm:items-end">
              <Seg<TimeFormatPref>
                value={p.time_format}
                options={[
                  { value: "h24", label: "24h" },
                  { value: "h12", label: "12h" },
                ]}
                onChange={(v) => m.mutate({ time_format: v })}
              />
              <span className="font-mono text-[11px] text-muted-foreground">
                e.g. {formatTimeWith(now, p.time_format)}
              </span>
            </div>
          </PrefRow>
        </div>
      </Panel>

      <Panel title="Behavior" sub="App-wide interaction preferences">
        <div className="flex flex-col gap-4">
          <PrefRow
            label="Default landing"
            hint="Where you start after signing in"
          >
            <Seg<DefaultLandingPref>
              value={p.default_landing}
              options={[
                { value: "dashboard", label: "Dashboard" },
                { value: "devices", label: "Devices" },
                { value: "topology", label: "Topology" },
              ]}
              onChange={(v) => m.mutate({ default_landing: v })}
            />
          </PrefRow>
          <PrefToggle
            label="Reduced motion"
            hint="Suppress animation in topology pulses, page transitions, and chart redraws"
            checked={p.reduced_motion}
            onCheckedChange={(v) => m.mutate({ reduced_motion: v })}
            id="pref-reduced-motion"
          />
        </div>
      </Panel>
    </div>
  )
}

// ── Notifications ─────────────────────────────────────────────────────

function NotificationsSection() {
  const qc = useQueryClient()
  const prefsQ = useQuery({
    queryKey: ["me", "preferences"],
    queryFn: getMyPreferences,
  })
  const m = useMutation({
    mutationFn: (patch: Partial<UserPreferences>) => setMyPreferences(patch),
    onSuccess: (data) => {
      qc.setQueryData(["me", "preferences"], data)
      toast.success("Notifications updated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  if (prefsQ.isLoading || !prefsQ.data) {
    return <Skeleton className="h-40 rounded-none" />
  }
  const p = prefsQ.data

  return (
    <div className="flex flex-col gap-6">
      <Panel title="Toasts" sub="Inline confirmation messages">
        <div className="flex flex-col gap-4">
          <PrefRow label="Position" hint="Where toasts appear on screen">
            <Seg<ToastPositionPref>
              value={p.toast_position}
              options={[
                { value: "top-left", label: "Top L" },
                { value: "top-right", label: "Top R" },
                { value: "bottom-left", label: "Bot L" },
                { value: "bottom-right", label: "Bot R" },
              ]}
              onChange={(v) => m.mutate({ toast_position: v })}
            />
          </PrefRow>
          <PrefToggle
            label="Play sound on toast"
            hint="Subtle chime when a toast appears. Off by default."
            checked={p.toast_sound}
            onCheckedChange={(v) => {
              m.mutate({ toast_sound: v })
              if (v) previewChime()
            }}
            id="pref-toast-sound"
          />
        </div>
      </Panel>

      <Panel
        title="Browser notifications"
        sub="System-level alerts via the Notifications API"
      >
        <PrefToggle
          label="Enable browser notifications"
          hint="One-time browser permission prompt the first time you enable this. We send a notification on key events (device offline / login from new location / admin alerts)."
          checked={p.browser_notifications}
          onCheckedChange={async (v) => {
            if (v && typeof Notification !== "undefined") {
              if (Notification.permission === "default") {
                const r = await Notification.requestPermission()
                if (r !== "granted") {
                  toast.error("Notification permission denied")
                  return
                }
              } else if (Notification.permission === "denied") {
                toast.error(
                  "Notifications are blocked in your browser settings"
                )
                return
              }
            }
            m.mutate({ browser_notifications: v })
          }}
          id="pref-browser-notifications"
        />
      </Panel>

      <Panel
        title="Email notifications"
        sub="Transactional alerts sent to the email on file"
      >
        <div className="flex flex-col gap-4">
          <PrefToggle
            label="New device added"
            hint="Email me whenever a new WireGuard device is registered to my account. Useful as a tripwire if someone enrolls a device without your knowledge."
            checked={p.email_on_new_device}
            onCheckedChange={(v) => m.mutate({ email_on_new_device: v })}
            id="pref-email-new-device"
          />
          <PrefToggle
            label="Approaching bandwidth quota"
            hint="Email me when this month's traffic crosses 80% of my cap. Skipped when no cap is set."
            checked={p.email_on_quota_warning}
            onCheckedChange={(v) => m.mutate({ email_on_quota_warning: v })}
            id="pref-email-quota"
          />
          <PrefToggle
            label="Security events"
            hint="Email me on sign-in from a new IP, password changes, 2FA enable/disable, and admin actions taken on my account. Default ON — opt out reduces signal during incidents."
            checked={p.email_on_security_event}
            onCheckedChange={(v) => m.mutate({ email_on_security_event: v })}
            id="pref-email-security"
          />
        </div>
      </Panel>

      <Panel
        title="Test"
        sub="Fires a sample notification with your current settings"
      >
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() =>
              notify.info("Test notification", {
                description:
                  "Position, chime, and browser alert reflect your settings.",
                important: true,
                id: "settings-test",
              })
            }
          >
            Send test
          </Button>
          <p className="self-center font-mono text-[11px] text-muted-foreground">
            Hide this tab before clicking to see a browser-level alert.
          </p>
        </div>
      </Panel>
    </div>
  )
}

// ── Small helpers ─────────────────────────────────────────────────────

function PrefRow({
  label,
  hint,
  children,
}: {
  label: string
  hint: string
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:justify-between sm:gap-6">
      <div className="min-w-0">
        <div className="text-[13px] font-medium text-foreground">{label}</div>
        <div className="font-mono text-[11px] text-muted-foreground">
          {hint}
        </div>
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  )
}

function PrefToggle({
  label,
  hint,
  checked,
  onCheckedChange,
  id,
}: {
  label: string
  hint: string
  checked: boolean
  onCheckedChange: (v: boolean) => void
  id: string
}) {
  return (
    <div className="flex items-start gap-3">
      <Switch
        id={id}
        checked={checked}
        onCheckedChange={(v) => onCheckedChange(!!v)}
      />
      <label htmlFor={id} className="min-w-0 flex-1 cursor-pointer">
        <div className="text-[13px] font-medium text-foreground">{label}</div>
        <div className="mt-0.5 font-mono text-[11px] leading-relaxed text-muted-foreground">
          {hint}
        </div>
      </label>
    </div>
  )
}
