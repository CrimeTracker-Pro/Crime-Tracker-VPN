import { useCallback, useEffect, useState } from "react"
import { useNavigate, useSearchParams } from "react-router"
import { toast } from "sonner"

import {
  AuthFooterRule,
  AuthForm,
  AuthHeading,
  AuthShell,
} from "@/components/layout/AuthShell"
import { Kbd } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import {
  InputOTP,
  InputOTPGroup,
  InputOTPSlot,
} from "@/components/ui/input-otp"
import { Label } from "@/components/ui/label"
import {
  ApiError,
  getMyPreferences,
  googleCallback,
  googleVerifyTotp,
  landingPath,
  me,
  type LoginResponse,
  type PublicUser,
} from "@/lib/api"
import { useAuth } from "@/stores/auth"

/**
 * Google's redirect lands here with `code` + `state`. We exchange them for a
 * session. If the account has 2FA enabled the backend returns `totp_required`
 * and leaves a pending-TOTP session (NOT a real one) — so we show the same
 * 6-digit prompt and complete via `/auth/google/verify-totp`.
 * The Google identity alone never bypasses 2FA.
 *
 * The OAuth `state` is single-use server-side, so the exchange must run once.
 * Module-scope dedupe handles React StrictMode's double-mount (shared in-flight
 * promise) and refresh/back (a sessionStorage marker). A second marker
 * remembers "awaiting 2FA" so the prompt survives a StrictMode remount instead
 * of falling through to a misleading "session expired".
 */
const inflight = new Map<string, Promise<LoginResponse>>()
const DONE_KEY_PREFIX = "oauth-state-done:"
const TOTP_KEY_PREFIX = "oauth-state-totp:"

export function GoogleCallbackPage() {
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const setUser = useAuth((s) => s.setUser)
  // Bad callback params are derivable straight from the URL; only errors
  // from the async exchange/verify need state.
  const googleError = params.get("error")
  const paramError = googleError
    ? `Google returned: ${googleError}`
    : !params.get("code") || !params.get("state")
      ? "Missing code or state in callback URL"
      : null
  const [asyncError, setAsyncError] = useState<string | null>(null)
  const error = asyncError ?? paramError
  const [needsTotp, setNeedsTotp] = useState(false)
  const [totpCode, setTotpCode] = useState("")
  const [verifying, setVerifying] = useState(false)

  // Drop the user into the app once we hold a *real* session.
  const finishSignIn = useCallback(
    async (user: PublicUser) => {
      setUser(user)
      toast.success(`Welcome, ${user.email}`)
      const prefs = await getMyPreferences().catch(() => null)
      navigate(prefs ? landingPath(prefs.default_landing) : "/app", {
        replace: true,
      })
    },
    [navigate, setUser]
  )

  useEffect(() => {
    const code = params.get("code")
    const state = params.get("state")
    if (!code || !state || params.get("error")) return

    let cancelled = false
    const fail = (msg: string) => {
      if (cancelled) return
      setAsyncError(msg)
      toast.error(msg)
    }

    ;(async () => {
      // Already exchanged this state (StrictMode remount, or a refresh).
      if (sessionStorage.getItem(DONE_KEY_PREFIX + state)) {
        // Mid-2FA: re-show the prompt instead of hitting /me, which 401s on a
        // pending-TOTP session. Survives StrictMode's remount.
        if (sessionStorage.getItem(TOTP_KEY_PREFIX + state)) {
          if (!cancelled) setNeedsTotp(true)
          return
        }
        try {
          const u = await me()
          if (!cancelled) await finishSignIn(u)
        } catch {
          fail("Sign-in session expired — please try again.")
        }
        return
      }

      // Share one exchange across StrictMode's double-mount.
      let promise = inflight.get(state)
      if (!promise) {
        promise = googleCallback({ code, state })
        inflight.set(state, promise)
      }

      try {
        const res = await promise
        sessionStorage.setItem(DONE_KEY_PREFIX + state, "1")
        if (cancelled) return
        if (res.totp_required) {
          sessionStorage.setItem(TOTP_KEY_PREFIX + state, "1")
          setNeedsTotp(true)
          return
        }
        await finishSignIn(res.user)
      } catch (e) {
        inflight.delete(state)
        fail(
          e instanceof ApiError ? e.message : "Couldn't complete Google sign-in"
        )
      }
    })()

    return () => {
      cancelled = true
    }
  }, [params, finishSignIn])

  const submitTotp = async () => {
    const code = totpCode.trim()
    if (code.length < 6 || verifying) return
    setVerifying(true)
    setAsyncError(null)
    try {
      const res = await googleVerifyTotp(code)
      const state = params.get("state")
      if (state) sessionStorage.removeItem(TOTP_KEY_PREFIX + state)
      await finishSignIn(res.user)
    } catch (e) {
      setAsyncError(
        e instanceof ApiError ? e.message : "Couldn't verify the code"
      )
      setVerifying(false)
    }
  }

  return (
    <AuthShell>
      {needsTotp ? (
        <AuthForm
          onSubmit={(e) => {
            e.preventDefault()
            void submitTotp()
          }}
        >
          <AuthHeading eyebrow="02 · Two-factor">
            Enter the 6-digit code.
          </AuthHeading>

          <div className="flex flex-col gap-1.5">
            <Label className="zv-eyebrow">6-digit code</Label>
            <InputOTP
              maxLength={6}
              value={totpCode}
              onChange={(v) => setTotpCode(v)}
              autoFocus
            >
              <InputOTPGroup>
                <InputOTPSlot index={0} />
                <InputOTPSlot index={1} />
                <InputOTPSlot index={2} />
                <InputOTPSlot index={3} />
                <InputOTPSlot index={4} />
                <InputOTPSlot index={5} />
              </InputOTPGroup>
            </InputOTP>
            <p className="font-mono text-[11px] text-muted-foreground">
              From your authenticator app or recovery code
            </p>
          </div>

          {error && (
            <p className="font-mono text-xs text-destructive">{error}</p>
          )}

          <Button
            type="submit"
            disabled={totpCode.length < 6 || verifying}
            size="lg"
          >
            {verifying ? "Verifying…" : "Verify"}
            <Kbd className="ml-2">↵</Kbd>
          </Button>

          <AuthFooterRule>
            <button
              type="button"
              onClick={() => navigate("/login", { replace: true })}
              className="hover:text-foreground"
            >
              ← Back to sign in
            </button>
          </AuthFooterRule>
        </AuthForm>
      ) : (
        <div className="flex w-[360px] max-w-full flex-col gap-5">
          <AuthHeading eyebrow="·· Signing in">
            {error ? "Sign-in failed." : "Finishing up…"}
          </AuthHeading>
          {error ? (
            <p className="font-mono text-xs text-destructive">{error}</p>
          ) : (
            <p className="font-mono text-xs text-muted-foreground">
              Completing Google authentication.
            </p>
          )}
          <AuthFooterRule>
            <button
              type="button"
              onClick={() => navigate("/login", { replace: true })}
              className="hover:text-foreground"
            >
              ← Back to sign in
            </button>
          </AuthFooterRule>
        </div>
      )}
    </AuthShell>
  )
}
