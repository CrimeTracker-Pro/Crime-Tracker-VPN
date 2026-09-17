/*
 * Crime Tracker VPN service worker.
 *
 * Scope is intentionally minimal: it exists to make the app installable as a
 * PWA and to display + handle clicks on OS notifications. There is no Web Push
 * and no offline caching — notifications are shown while a session is live
 * (the page calls `registration.showNotification`), and a VPN dashboard needs
 * the live API anyway, so we deliberately don't intercept `fetch` (that keeps
 * Vite HMR and API calls untouched).
 */

const SW_VERSION = "zv-sw-v1"

self.addEventListener("install", () => {
  // Activate a new SW immediately instead of waiting for all tabs to close.
  self.skipWaiting()
})

self.addEventListener("activate", (event) => {
  // Start controlling open pages right away so `showNotification` is available.
  event.waitUntil(self.clients.claim())
})

// Clicking a notification focuses an existing app window (navigating it to the
// notification's target) or opens a new one.
self.addEventListener("notificationclick", (event) => {
  event.notification.close()
  const data = event.notification.data || {}
  const target = typeof data.url === "string" && data.url ? data.url : "/app"

  event.waitUntil(
    (async () => {
      const clients = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      })
      for (const client of clients) {
        // Reuse a same-origin window if one is open.
        if ("focus" in client) {
          if (typeof client.navigate === "function") {
            try {
              await client.navigate(target)
            } catch {
              /* cross-origin / detached — fall through to focus */
            }
          }
          return client.focus()
        }
      }
      if (self.clients.openWindow) return self.clients.openWindow(target)
      return undefined
    })(),
  )
})

// Keep a reference so bundlers/linters don't flag the version constant as dead;
// also handy when debugging which SW is active in DevTools → Application.
self.SW_VERSION = SW_VERSION
