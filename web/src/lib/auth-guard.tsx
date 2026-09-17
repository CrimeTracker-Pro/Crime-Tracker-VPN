/* eslint-disable react-refresh/only-export-components -- guard components +
   the bootstrap hook form one auth boundary consumed by the route table; a
   full reload on edit is correct here, not a fast-refresh regression. */
import { useEffect } from "react"
import { Navigate } from "react-router"

import { LogoLoader } from "@/components/swiss"
import { ApiError, me } from "@/lib/api"
import { useAuth } from "@/stores/auth"

export function useBootstrapAuth() {
  const { setUser, setLoading } = useAuth()

  useEffect(() => {
    let alive = true
    let attempt = 0
    setLoading(true)

    const tryMe = () => {
      me()
        .then((u) => {
          if (alive) {
            setUser(u)
            setLoading(false)
          }
        })
        .catch((e) => {
          if (!alive) return
          if (e instanceof ApiError) {
            // Definitive 401 from the server: the session really is gone,
            // drop the user so the route guard can redirect to /login.
            if (e.status === 401) {
              setUser(null)
              setLoading(false)
              return
            }
            // Other API errors (500, maintenance, etc.): leave the user
            // state alone and stop the loading spinner; ProtectedRoute will
            // route based on whatever was last known.
            setLoading(false)
            return
          }
          // Network / fetch failure (typically the dev api is restarting):
          // back off and retry a few times before giving up. This avoids
          // the "every cargo restart bounces me to /login" loop.
          attempt += 1
          if (attempt <= 4) {
            const delayMs = Math.min(4000, 250 * 2 ** attempt)
            setTimeout(tryMe, delayMs)
            return
          }
          setLoading(false)
        })
    }
    tryMe()

    return () => {
      alive = false
    }
  }, [setUser, setLoading])
}

/** Root "/" entry. The deployed app has no marketing landing — wait for the
 *  auth bootstrap to settle (showing the branded loader so signed-in users
 *  aren't flashed the login form), then send authenticated users to the
 *  dashboard and everyone else to the login page, which is the front door. */
export function HomeRedirect() {
  const { user, loading } = useAuth()
  if (loading) return <LogoLoader caption="loading" />
  return <Navigate to={user ? "/app" : "/login"} replace />
}

export function ProtectedRoute({ children }: { children: React.ReactNode }) {
  const { user, loading } = useAuth()
  if (loading) {
    // Bootstrapping the session cookie — usually < 150 ms. Render the
    // animated brand loader so even sub-second boots show consistent
    // brand chrome instead of skeleton bars.
    return <LogoLoader caption="restoring session" />
  }
  if (!user) return <Navigate to="/login" replace />
  return <>{children}</>
}

export function AdminRoute({ children }: { children: React.ReactNode }) {
  const { user, loading } = useAuth()
  if (loading) return <LogoLoader caption="verifying" />
  if (!user) return <Navigate to="/login" replace />
  if (user.role !== "admin") return <Navigate to="/app" replace />
  return <>{children}</>
}

/** Gate the /app/devices/{id} route behind the global "Hide device detail"
 *  user-policy toggle. Non-admins get bounced back to the device list when
 *  the policy is on; admins always pass through so they can still inspect
 *  any device. The auth-store snapshot is hydrated from /me, login, and
 *  verify-email, so this works from first paint without an extra fetch. */
export function DeviceDetailRoute({ children }: { children: React.ReactNode }) {
  const user = useAuth((s) => s.user)
  if (user && user.role !== "admin" && user.user_policy?.hide_device_detail) {
    return <Navigate to="/app/devices" replace />
  }
  return <>{children}</>
}
