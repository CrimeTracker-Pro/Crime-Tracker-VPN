import { IconLoader2 } from "@tabler/icons-react"
import { useEffect, useState } from "react"
import { Link, useSearchParams } from "react-router"
import { toast } from "sonner"

import {
  AuthForm,
  AuthFooterRule,
  AuthHeading,
  AuthShell,
} from "@/components/layout/AuthShell"
import { CodeBlock, Pill } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { ApiError, googleStartUrl, verifyEmail } from "@/lib/api"

type Status = "pending" | "ok" | "fail"

export function VerifyEmailPage() {
  const [params] = useSearchParams()
  const token = params.get("token")
  // A missing token is derivable straight from the URL; only the async
  // verification result needs state.
  const [result, setResult] = useState<{
    status: Status
    message: string
  } | null>(null)
  const status: Status = token ? (result?.status ?? "pending") : "fail"
  const message = token
    ? (result?.message ?? "")
    : "Missing verification token."

  useEffect(() => {
    if (!token) return
    let alive = true
    verifyEmail(token)
      .then(() => {
        if (!alive) return
        setResult({
          status: "ok",
          message: "Invitation verified. Continue with Google to access the VPN.",
        })
        toast.success("Invitation verified")
      })
      .catch((e) => {
        if (alive) {
          setResult({
            status: "fail",
            message: e instanceof ApiError ? e.message : "Verification failed",
          })
        }
      })
    return () => {
      alive = false
    }
  }, [token])

  return (
    <AuthShell>
      <AuthForm>
        <AuthHeading eyebrow="02 · Verify email">
          {status === "pending"
            ? "Verifying…"
            : status === "ok"
              ? "Check passed."
              : "Verification failed."}
        </AuthHeading>

        <div className="flex items-center gap-3">
          <Pill
            tone={status === "ok" ? "ok" : status === "fail" ? "err" : "warn"}
          >
            {status === "ok"
              ? "verified"
              : status === "fail"
                ? "failed"
                : "pending"}
          </Pill>
          {status === "pending" && (
            <IconLoader2 className="size-4 animate-spin text-muted-foreground" />
          )}
        </div>

        {!token ? (
          <p className="text-sm leading-relaxed text-muted-foreground">
            We sent a token-link to your inbox. Click it to finish creating your
            account.
          </p>
        ) : (
          <p className="text-sm leading-relaxed">{message}</p>
        )}

        {!token && (
          <CodeBlock>{`From: ZeroVPN <noreply@your-domain.tld>
Subject: Verify your account

→ https://your-host.tld/verify-email?token=eyJhbGciOi…  (24h)`}</CodeBlock>
        )}

        {status === "fail" && (
          <div className="flex gap-2">
            <Button asChild>
              <Link to="/login">Continue to sign in</Link>
            </Button>
          </div>
        )}

        {status === "ok" && (
          <Button type="button" onClick={() => { window.location.href = googleStartUrl }}>
            Continue with Google
          </Button>
        )}

        <AuthFooterRule>
          <Link to="/login" className="hover:text-foreground">
            ← Back to sign in
          </Link>
        </AuthFooterRule>
      </AuthForm>
    </AuthShell>
  )
}
