import { AuthFooterRule, AuthForm, AuthHeading, AuthShell } from "@/components/layout/AuthShell"
import { Button } from "@/components/ui/button"
import { googleStartUrl } from "@/lib/api"

/** Invitation-only portal: Google OAuth is the sole authentication method. */
export function LoginPage() {
  return (
    <AuthShell>
      <AuthForm>
        <AuthHeading eyebrow="01 · Sign in">Welcome back.</AuthHeading>
        <p className="text-sm leading-relaxed text-muted-foreground">
          Access is limited to invited users. Continue with the Google account
          that received your invitation.
        </p>
        <Button type="button" size="lg" onClick={() => { window.location.href = googleStartUrl }}>
          <GoogleGlyph />
          Continue with Google
        </Button>
        <AuthFooterRule />
      </AuthForm>
    </AuthShell>
  )
}

function GoogleGlyph() {
  return (
    <svg viewBox="0 0 18 18" className="mr-1 h-4 w-4" aria-hidden>
      <path fill="#4285F4" d="M17.64 9.2c0-.637-.057-1.251-.164-1.84H9v3.481h4.844a4.14 4.14 0 0 1-1.796 2.717v2.258h2.908c1.702-1.567 2.684-3.874 2.684-6.615z" />
      <path fill="#34A853" d="M9 18c2.43 0 4.467-.806 5.956-2.18l-2.908-2.259c-.806.54-1.837.86-3.048.86-2.344 0-4.328-1.584-5.036-3.711H.957v2.332A8.997 8.997 0 0 0 9 18z" />
      <path fill="#FBBC05" d="M3.964 10.71A5.41 5.41 0 0 1 3.682 9c0-.593.102-1.17.282-1.71V4.958H.957A8.996 8.996 0 0 0 0 9c0 1.452.348 2.827.957 4.042l3.007-2.332z" />
      <path fill="#EA4335" d="M9 3.579c1.322 0 2.508.455 3.441 1.35l2.581-2.581C13.463.892 11.43 0 9 0 5.488 0 2.44 2.017.957 4.958l3.007 2.332C4.672 5.163 6.656 3.579 9 3.579z" />
    </svg>
  )
}
