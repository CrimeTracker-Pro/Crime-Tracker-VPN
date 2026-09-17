import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { IconCopy, IconDownload, IconLogout, IconX } from "@tabler/icons-react"
import { motion } from "motion/react"
import { useState } from "react"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { CopyableCode } from "@/components/CopyableCode"
import { RelativeTime } from "@/components/RelativeTime"
import { Kbd, Panel, Pill } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  InputOTP,
  InputOTPGroup,
  InputOTPSlot,
} from "@/components/ui/input-otp"
import { Label } from "@/components/ui/label"
import {
  ApiError,
  mySessions,
  revokeOtherSessions,
  revokeSession,
  totpDisable,
  totpRegenerateRecovery,
  totpEnable,
  totpSetup,
} from "@/lib/api"
import { copyText } from "@/lib/clipboard"
import { useAuth } from "@/stores/auth"

/** Security-management content embedded by the unified `/app/settings`
 *  page (Security tab). Owns TOTP enrollment, recovery-code download,
 *  and the disable-2FA flow. */
export function SecuritySections() {
  const user = useAuth((s) => s.user)
  const setUser = useAuth((s) => s.setUser)
  const qc = useQueryClient()

  const [setupData, setSetupData] = useState<{
    secret: string
    qr_svg: string
  } | null>(null)
  const [code, setCode] = useState("")
  const [recoveryCodes, setRecoveryCodes] = useState<string[] | null>(null)

  const setupM = useMutation({
    mutationFn: totpSetup,
    onSuccess: (d) => setSetupData({ secret: d.secret, qr_svg: d.qr_svg }),
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const enableM = useMutation({
    mutationFn: () => totpEnable(setupData!.secret, code.trim()),
    onSuccess: (d) => {
      setRecoveryCodes(d.recovery_codes)
      setSetupData(null)
      setCode("")
      // Flip the auth-store flag so the status pill + form swap to
      // the "enabled" state instantly — no round-trip to /me needed.
      if (user) setUser({ ...user, totp_enabled: true })
      void qc.invalidateQueries()
      toast.success("2FA enabled")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const regenM = useMutation({
    mutationFn: (c: string) => totpRegenerateRecovery(c.trim()),
    onSuccess: (d) => {
      // Reuse the shown-once recovery panel (copy + download included).
      setRecoveryCodes(d.recovery_codes)
      toast.success("Recovery codes regenerated — previous codes are dead")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const disableM = useMutation({
    mutationFn: (c: string) => totpDisable(c.trim()),
    onSuccess: () => {
      if (user) setUser({ ...user, totp_enabled: false })
      void qc.invalidateQueries()
      toast.success("2FA disabled")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const totpEnabled = user?.totp_enabled ?? false

  return (
    <div className="flex flex-col gap-6">
      <Panel
        title="Two-factor authentication"
        sub="time-based one-time password (TOTP)"
      >
        {!setupData && !recoveryCodes && (
          <div className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-3">
              {totpEnabled ? (
                <Pill tone="ok">Enabled</Pill>
              ) : (
                <Pill tone="warn">Disabled</Pill>
              )}
              <span className="text-xs text-muted-foreground">
                {totpEnabled
                  ? "An authenticator app is configured for this account. Sign-ins require a 6-digit code."
                  : "Required for admins. Recommended for everyone."}
              </span>
            </div>

            {totpEnabled ? (
              // 2FA already on — both code-gated actions rendered inline
              // (no need to hide them behind a <details> disclosure).
              <div className="flex flex-col gap-3">
                <CodeConfirmForm
                  hint="Lost your recovery codes? Enter a current authenticator code to mint a fresh set — every previous code stops working."
                  cta="Regenerate recovery codes"
                  pendingCta="Regenerating…"
                  variant="outline"
                  onSubmit={(c) => regenM.mutate(c)}
                  pending={regenM.isPending}
                />
                <CodeConfirmForm
                  hint="Enter a current authenticator code to confirm. Recovery codes also work here."
                  cta="Disable 2FA"
                  pendingCta="Disabling…"
                  onSubmit={(c) => disableM.mutate(c)}
                  pending={disableM.isPending}
                />
              </div>
            ) : (
              <div className="flex gap-2">
                <Button
                  onClick={() => setupM.mutate()}
                  disabled={setupM.isPending}
                >
                  {setupM.isPending ? "Setting up…" : "Enable 2FA"}
                </Button>
              </div>
            )}
          </div>
        )}

        {setupData && (
          <motion.div
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.18 }}
            className="flex flex-col gap-4"
          >
            <p className="text-sm">
              Scan this QR with your authenticator app, then enter the 6-digit
              code below.
            </p>

            {/* Two-column layout: QR locked to a square on the left,
                manual-entry + OTP input stretch to fill the rest on the
                right. Collapses to stacked on narrow viewports so the
                160px QR doesn't crowd the form. The SVG is painted
                directly into the box; the `.zv-qr-box svg` rule scales
                it to fit, overriding the qrcode crate's 256px intrinsic
                size. */}
            <div className="grid items-start gap-5 sm:grid-cols-[auto_1fr]">
              <div
                className="zv-qr-box size-40 shrink-0 bg-card"
                dangerouslySetInnerHTML={{ __html: setupData.qr_svg }}
              />

              <div className="flex min-w-0 flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label className="zv-eyebrow">Or enter manually</Label>
                  <CopyableCode value={setupData.secret} truncate />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label className="zv-eyebrow">Verification code</Label>
                  <InputOTP maxLength={6} value={code} onChange={setCode}>
                    <InputOTPGroup>
                      <InputOTPSlot index={0} />
                      <InputOTPSlot index={1} />
                      <InputOTPSlot index={2} />
                      <InputOTPSlot index={3} />
                      <InputOTPSlot index={4} />
                      <InputOTPSlot index={5} />
                    </InputOTPGroup>
                  </InputOTP>
                </div>
              </div>
            </div>

            <div className="flex gap-2">
              <Button
                onClick={() => enableM.mutate()}
                disabled={enableM.isPending || code.length < 6}
              >
                {enableM.isPending ? "Verifying…" : "Verify & enable"}
              </Button>
              <Button variant="ghost" onClick={() => setSetupData(null)}>
                Cancel
              </Button>
            </div>
          </motion.div>
        )}

        {recoveryCodes && (
          <motion.div
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.18 }}
            className="flex flex-col gap-3"
          >
            <div>
              <p className="text-sm font-medium">
                Save these recovery codes — they're shown only once.
              </p>
              <p className="font-mono text-xs text-muted-foreground">
                Each code can be used once if you lose your authenticator.
              </p>
            </div>
            <div className="grid grid-cols-2 gap-2">
              {recoveryCodes.map((c) => (
                <Kbd key={c} className="text-center">
                  {c}
                </Kbd>
              ))}
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  if (copyText(recoveryCodes.join("\n")))
                    toast.success("Recovery codes copied")
                  else toast.error("Failed to copy")
                }}
              >
                <IconCopy className="size-3.5" />
                Copy all
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() =>
                  downloadRecoveryCodes(recoveryCodes, user?.email)
                }
              >
                <IconDownload className="size-3.5" />
                Download .txt
              </Button>
              <Button size="sm" onClick={() => setRecoveryCodes(null)}>
                I've saved them
              </Button>
            </div>
          </motion.div>
        )}
      </Panel>

      <Panel
        title="Active sessions"
        sub="Every browser / device this account is currently signed in on"
        flush
      >
        <SessionsList />
        <div className="border-t border-border p-4">
          <SignOutEverywherePanel />
        </div>
      </Panel>
    </div>
  )
}

/** Compact "Chrome · macOS" style label from a raw User-Agent. Best-effort
 *  string sniffing — unknown agents fall back to the raw UA (truncated). */
function uaSummary(ua: string | null): string {
  if (!ua) return "Unknown client"
  const browser = /edg\//i.test(ua)
    ? "Edge"
    : /firefox\//i.test(ua)
      ? "Firefox"
      : /chrome|crios/i.test(ua)
        ? "Chrome"
        : /safari/i.test(ua)
          ? "Safari"
          : /curl|wget|python|httpie/i.test(ua)
            ? "CLI / script"
            : null
  const os = /android/i.test(ua)
    ? "Android"
    : /iphone|ipad|ios/i.test(ua)
      ? "iOS"
      : /mac os x|macintosh/i.test(ua)
        ? "macOS"
        : /windows/i.test(ua)
          ? "Windows"
          : /linux/i.test(ua)
            ? "Linux"
            : null
  if (browser && os) return `${browser} · ${os}`
  if (browser || os) return browser ?? os ?? ""
  return ua.length > 40 ? `${ua.slice(0, 40)}…` : ua
}

/** Live session rows with per-session revoke. The current session is
 *  pinned with a pill instead of a revoke button (use logout for that). */
function SessionsList() {
  const qc = useQueryClient()
  const q = useQuery({
    queryKey: ["me", "sessions"],
    queryFn: mySessions,
    refetchInterval: 30_000,
  })
  const revokeM = useMutation({
    mutationFn: (id: string) => revokeSession(id),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["me", "sessions"] })
      toast.success("Session revoked")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  const sessions = q.data ?? []
  return (
    <div className="flex flex-col">
      {q.isLoading && (
        <p className="p-4 font-mono text-xs text-muted-foreground">
          Loading sessions…
        </p>
      )}
      {sessions.map((s) => (
        <div
          key={s.id}
          className="flex items-center justify-between gap-3 border-b border-border px-4 py-2.5 last:border-b-0"
        >
          <div className="flex min-w-0 flex-col gap-0.5">
            <div className="flex items-center gap-2">
              <span className="text-sm font-medium">
                {uaSummary(s.user_agent)}
              </span>
              {s.current && (
                <Pill tone="ok" dot={false}>
                  this session
                </Pill>
              )}
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">
              {s.ip ? s.ip.replace(/\/(32|128)$/, "") : "unknown ip"}
              <span className="px-1">·</span>
              active <RelativeTime value={s.last_seen_at} />
              <span className="px-1">·</span>
              signed in <RelativeTime value={s.created_at} />
            </span>
          </div>
          {!s.current && (
            <Button
              size="sm"
              variant="outline"
              disabled={revokeM.isPending}
              onClick={() => revokeM.mutate(s.id)}
              title="Sign this session out"
            >
              <IconX className="size-3.5" />
              Revoke
            </Button>
          )}
        </div>
      ))}
      {!q.isLoading && sessions.length === 0 && (
        <p className="p-4 font-mono text-xs text-muted-foreground">
          No session metadata yet — rows appear as sessions make requests.
        </p>
      )}
    </div>
  )
}

/**
 * "Sign out everywhere else" panel. Calls the user-side
 * `revoke-all-sessions` endpoint which bumps the password watermark on
 * the server. Every other open session for this account fails the auth
 * extractor's pw-version check on its very next request and is kicked
 * back to /login. The current session stays alive — the server re-syncs
 * our snapshot in the same call.
 */
function SignOutEverywherePanel() {
  const [open, setOpen] = useState(false)
  const m = useMutation({
    mutationFn: revokeOtherSessions,
    onSuccess: () => {
      setOpen(false)
      toast.success("All other sessions signed out")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  return (
    <>
      <div className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">
          Use this if you forgot to sign out somewhere, suspect a session you
          don't recognise, or just rotated your password externally. You stay
          signed in here.
        </p>
        <div>
          <Button
            variant="outline"
            onClick={() => setOpen(true)}
            disabled={m.isPending}
          >
            <IconLogout className="size-4" />
            Sign out everywhere else
          </Button>
        </div>
      </div>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title="Sign out of all other sessions?"
        description="Every other open session for your account is invalidated immediately. This session stays active. The action is logged in the audit trail."
        confirmLabel="Sign out everywhere"
        destructive
        pending={m.isPending}
        onConfirm={() => m.mutate()}
      />
    </>
  )
}


/** Trigger a browser download of the recovery codes as a plain .txt
 *  file. We render the codes as a Blob, build an `<a download>`,
 *  click it programmatically, then revoke the object URL. No copy is
 *  retained server-side past this single response; the user *must*
 *  save them now. Filename includes a date stamp + the user's email
 *  local-part so multiple downloads (e.g. testing) don't collide in
 *  the Downloads folder. */
function downloadRecoveryCodes(codes: string[], email: string | undefined) {
  const today = new Date().toISOString().slice(0, 10)
  const localPart = email?.split("@")[0]?.replace(/[^a-z0-9_-]/gi, "") || "user"
  const filename = `crimetracker-vpn-recovery-codes-${localPart}-${today}.txt`
  const body = [
    "Crime Tracker VPN — Two-Factor Authentication Recovery Codes",
    `Account: ${email ?? "(unknown)"}`,
    `Generated: ${new Date().toISOString()}`,
    "",
    "Each code below can be used ONCE to sign in if you lose access to",
    "your authenticator app. Treat them like passwords — store them",
    "somewhere safe (password manager, encrypted note, printed copy in",
    "a desk drawer). Anyone with one of these codes can sign in as you.",
    "",
    ...codes,
    "",
  ].join("\n")
  const blob = new Blob([body], { type: "text/plain;charset=utf-8" })
  const url = URL.createObjectURL(blob)
  const a = document.createElement("a")
  a.href = url
  a.download = filename
  a.rel = "noopener"
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
  toast.success("Recovery codes downloaded")
}

/** Code-gated action row: an authenticator-code input beside a button.
 *  Shared by "Disable 2FA" and "Regenerate recovery codes" so both are
 *  protected the same way (a stolen session alone isn't enough). */
function CodeConfirmForm({
  hint,
  cta,
  pendingCta,
  variant = "destructive",
  onSubmit,
  pending,
}: {
  hint: string
  cta: string
  pendingCta: string
  variant?: "destructive" | "outline"
  onSubmit: (code: string) => void
  pending?: boolean
}) {
  const [c, setC] = useState("")
  return (
    <div className="mt-1 flex flex-col gap-2 border-l border-border pl-3">
      <p className="font-mono text-[11px] text-muted-foreground">{hint}</p>
      <div className="flex flex-wrap gap-2">
        <Input
          value={c}
          onChange={(e) => setC(e.target.value)}
          placeholder="6-digit code"
          autoComplete="one-time-code"
          inputMode="numeric"
          className="w-44 font-mono"
        />
        <Button
          size="sm"
          variant={variant}
          onClick={() => onSubmit(c)}
          disabled={pending || c.length < 6}
        >
          {pending ? pendingCta : cta}
        </Button>
      </div>
    </div>
  )
}
