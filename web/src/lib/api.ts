/**
 * Thin fetch wrapper. Adds versioned base path, JSON serialization,
 * normalized errors, and credentials inclusion for session cookies.
 */

const BASE = (import.meta.env.VITE_API_BASE ?? "/api/v1").replace(/\/$/, "")

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly requestId?: string

  constructor(
    status: number,
    code: string,
    message: string,
    requestId?: string
  ) {
    super(message)
    this.name = "ApiError"
    this.status = status
    this.code = code
    this.requestId = requestId
  }
}

interface ApiErrorBody {
  error?: { code?: string; message?: string; request_id?: string }
}

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const url = `${BASE}${path.startsWith("/") ? path : `/${path}`}`
  const headers = new Headers(init?.headers)
  if (init?.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json")
  }
  headers.set("Accept", "application/json")

  const res = await fetch(url, { ...init, headers, credentials: "include" })

  if (!res.ok) {
    let body: ApiErrorBody = {}
    try {
      body = (await res.json()) as ApiErrorBody
    } catch {
      // ignore
    }
    throw new ApiError(
      res.status,
      body.error?.code ?? `http_${res.status}`,
      body.error?.message ?? res.statusText,
      body.error?.request_id
    )
  }
  if (res.status === 204) return undefined as T
  return (await res.json()) as T
}

// --- types ---------------------------------------------------------------

export type UserRole = "admin" | "user"
export type DeviceOs =
  | "ios"
  | "android"
  | "macos"
  | "windows"
  | "linux"
  | "other"
export type DeviceType =
  | "phone"
  | "tablet"
  | "laptop"
  | "desktop"
  | "tv"
  | "router"
  | "watch"
  | "iot"
  | "server"
  | "other"
export type DeviceStatus = "active" | "paused" | "revoked"
export type UserStatus =
  | "active"
  | "suspended"
  | "pending_verification"
  | "deleted"

export interface PublicUser {
  id: string
  email: string
  role: UserRole
  /** True when the user has finished TOTP enrollment. Surfaced by
   *  `/me` and the login / verify-email responses so the Security
   *  page can auto-detect 2FA state without an extra round-trip. */
  totp_enabled: boolean
  /** True when the active session is an admin impersonating this account. */
  is_impersonated?: boolean
  /** Email of the admin who initiated impersonation. Only present when
   *  `is_impersonated` is true. */
  impersonator_email?: string
  /** Admin-set global toggles governing what *non-admin* users see in the
   *  user-facing app. Admins are exempt — gate logic should always be
   *  `policy.X && user.role !== "admin"`. Present on every auth-store
   *  hydrate (login / verify / /me) so route guards work from first paint. */
  user_policy: UserPolicySnapshot
}

export interface UserPolicySnapshot {
  /** When true, hide /app/devices/{id} from non-admin users. */
  hide_device_detail: boolean
}

export interface LoginResponse {
  user: PublicUser
  must_change_password: boolean
  totp_required: boolean
}

export interface PublicDevice {
  id: string
  user_id: string
  name: string
  os: DeviceOs
  device_type: DeviceType
  public_key: string
  allocated_ip: string
  status: DeviceStatus
  server_id: string
  dns_names: string[]
  dns_override: string[] | null
  last_handshake_at: string | null
  /** Public host:port the peer last connected from, as seen by the WG
   *  poller. `null` until the first handshake; persists as the last
   *  endpoint after the peer goes offline. Only populated on the
   *  list / get responses (not create / rotate). */
  last_peer_endpoint: string | null
  last_peer_endpoint_at: string | null
  created_at: string
  /** All-time bytes received / sent, from the persisted hourly bandwidth
   *  rollups. Survives reloads (unlike the session-local live counters in
   *  the WS store). `0` until the device transmits. Only on list / get. */
  total_rx_bytes: number
  total_tx_bytes: number
  /** Per-device monthly byte cap (null/0 = no device cap; account cap still
   *  applies). Admin-set. Only on list / get responses. */
  monthly_byte_cap: number | null
  /** This device's own bandwidth used this monthly cycle. list / get only. */
  current_month_bytes: number
  /** True when the quota sweep auto-paused this device (device or account
   *  cap), vs. a manual pause. */
  auto_paused: boolean
}

export interface CreatedDevice {
  device: PublicDevice
  config: string
  qr_svg: string
}

// --- endpoints -----------------------------------------------------------

export const register = (body: { email: string; password: string }) =>
  apiFetch<{ status: string }>("/auth/register", {
    method: "POST",
    body: JSON.stringify(body),
  })

export const login = (body: {
  email: string
  password: string
  totp_code?: string
}) =>
  apiFetch<LoginResponse>("/auth/login", {
    method: "POST",
    body: JSON.stringify(body),
  })

export const logout = () =>
  apiFetch<{ status: string }>("/auth/logout", { method: "POST" })

/**
 * URL that kicks off Google OAuth. Used as a top-level navigation target
 * (window.location = googleStartUrl) — the backend 302s to Google's
 * consent screen, which can only happen at the window level, not from
 * inside a fetch().
 */
export const googleStartUrl = `${BASE}/auth/google/start`

/** Complete the Google OAuth round-trip — called by the SPA's
 *  `/google/callback` page with the `code` + `state` Google appended to
 *  the redirect URL. Returns the same shape as `login`. */
export const googleCallback = (body: { code: string; state: string }) =>
  apiFetch<LoginResponse>("/auth/google/callback", {
    method: "POST",
    body: JSON.stringify(body),
  })

/** Second leg of a Google sign-in when the account has 2FA enabled — the
 *  callback returned `totp_required`, and this submits the TOTP/recovery code
 *  against the session's pending-2FA marker to finish authenticating. */
export const googleVerifyTotp = (totp_code: string) =>
  apiFetch<LoginResponse>("/auth/google/verify-totp", {
    method: "POST",
    body: JSON.stringify({ totp_code }),
  })

export const me = () => apiFetch<PublicUser>("/me")

/** Public-safe info about the user's WG server — used by the create-device
 * dialog to pre-fill defaults (DNS, split-tunnel CIDR) and render hints
 * ("must be inside <cidr>"). */
export interface MyServerInfo {
  cidr: string
  dns_servers: string[]
  endpoint_host: string
  endpoint_port: number
  mtu: number
}

export const meServer = () => apiFetch<MyServerInfo>("/me/server")

/** The signed-in user's account-level monthly bandwidth quota — usage, cap,
 *  and the next reset. Powers the dashboard "Quota" card. `monthly_byte_cap`
 *  is `null` when the account is uncapped (unlimited). */
export interface MyUsage {
  current_month_bytes: number
  monthly_byte_cap: number | null
  quota_resets_at: string
}

export const myUsage = () => apiFetch<MyUsage>("/me/usage")

/** One row in the user's own activity log — the self-service equivalent of
 *  the admin audit log, scoped to actions the user took or admin actions on
 *  their account. */
export interface MyActivityRow {
  id: number
  /** Dotted action key, e.g. `device.created`, `auth.login`. */
  action: string
  target_type: string | null
  target_id: string | null
  metadata: unknown
  created_at: string
}

export interface MyActivityPage {
  items: MyActivityRow[]
  total: number
}

export const listMyActivity = (limit = 50, offset = 0) =>
  apiFetch<MyActivityPage>(`/me/activity?limit=${limit}&offset=${offset}`)

// ── User preferences ──────────────────────────────────────────────────
// Settings-page values the server persists per-user so they sync across
// signed-in sessions. Appearance/theme stay client-local (localStorage)
// to avoid a flash of wrong paint on first render.

export type UnitsPref = "bps" | "Bps"
export type DateFormatPref = "iso" | "us" | "eu"
export type TimeFormatPref = "h24" | "h12"
export type DefaultLandingPref = "dashboard" | "devices" | "topology"
export type ToastPositionPref =
  | "top-left"
  | "top-center"
  | "top-right"
  | "bottom-left"
  | "bottom-center"
  | "bottom-right"
/** Visual theme variant. Each ships its own light + dark color tokens,
 *  font stack, radius, and spacing scale. Orthogonal to the light/dark
 *  mode toggle (stored client-side). */
type ThemePref = "swiss" | "brutalist" | "terminal" | "editorial" | "soft"

export interface UserPreferences {
  units: UnitsPref
  date_format: DateFormatPref
  time_format: TimeFormatPref
  reduced_motion: boolean
  default_landing: DefaultLandingPref
  toast_position: ToastPositionPref
  toast_sound: boolean
  browser_notifications: boolean
  /** Email when a new WG device is added to the account. */
  email_on_new_device: boolean
  /** Email when monthly bandwidth crosses 80% of the cap. */
  email_on_quota_warning: boolean
  /** Email for security-relevant events (new IP sign-in, password
   *  change, 2FA enable/disable, admin actions on the account). */
  email_on_security_event: boolean
  /** Visual theme variant. */
  theme: ThemePref
}

export const getMyPreferences = () =>
  apiFetch<UserPreferences>("/me/preferences")

export const setMyPreferences = (patch: Partial<UserPreferences>) =>
  apiFetch<UserPreferences>("/me/preferences", {
    method: "PUT",
    body: JSON.stringify(patch),
  })

/** Map a saved `default_landing` preference to its in-app route. The
 *  sign-in flows (password + Google) call this so a user lands on the
 *  screen they chose in Settings instead of always the dashboard. */
export function landingPath(pref: DefaultLandingPref): string {
  switch (pref) {
    case "devices":
      return "/app/devices"
    case "topology":
      return "/app/topology"
    default:
      return "/app"
  }
}

// ── Topology positions ─────────────────────────────────────────────────
// Per-user saved arrangement for the live-topology drag UI. Round-trip
// shape matches what we keep in localStorage: a flat {node_id: {x, y}}.

export interface TopologyPosition {
  x: number
  y: number
}

export interface TopologyPositionsResponse {
  positions: Record<string, TopologyPosition>
}

export const getMyTopology = () =>
  apiFetch<TopologyPositionsResponse>("/me/topology")

export const setMyTopology = (positions: Record<string, TopologyPosition>) =>
  apiFetch<{ status: string; count: number }>("/me/topology", {
    method: "PUT",
    body: JSON.stringify({ positions }),
  })

/** Verify-email response. The server upgrades the caller's session as
 *  part of a successful verify so the frontend can navigate straight to
 *  /app without an additional sign-in hop. */
export interface VerifyEmailResponse {
  status: string
  user: PublicUser
}
export const verifyEmail = (token: string) =>
  apiFetch<VerifyEmailResponse>("/auth/verify-email", {
    method: "POST",
    body: JSON.stringify({ token }),
  })

export const resendVerify = (email: string) =>
  apiFetch<{ status: string }>("/auth/resend-verify", {
    method: "POST",
    body: JSON.stringify({ email }),
  })

export const forgotPassword = (email: string) =>
  apiFetch<{ status: string }>("/auth/forgot-password", {
    method: "POST",
    body: JSON.stringify({ email }),
  })

export const resetPassword = (token: string, new_password: string) =>
  apiFetch<{ status: string }>("/auth/reset-password", {
    method: "POST",
    body: JSON.stringify({ token, new_password }),
  })

/** Authenticated change-password. Used by the in-app Settings → Change password
 *  panel — keeps the current session alive while invalidating every
 *  other session for this user. */
export const changePassword = (
  current_password: string,
  new_password: string
) =>
  apiFetch<{ status: string }>("/me/change-password", {
    method: "POST",
    body: JSON.stringify({ current_password, new_password }),
  })

/** "Sign out everywhere" — invalidates every session for the current
 *  user except the one that called this endpoint. The current session
 *  stays alive (the server re-syncs the watermark snapshot in-place). */
export const revokeOtherSessions = () =>
  apiFetch<{ status: string }>("/me/sessions/revoke-all", { method: "POST" })

/** One live session, as shown on the Security page. `id` is a surrogate —
 *  the real session id never leaves the server. */
export interface MySession {
  id: string
  ip: string | null
  user_agent: string | null
  created_at: string
  last_seen_at: string
  current: boolean
}

export const mySessions = () => apiFetch<MySession[]>("/me/sessions")

/** Revoke one session. Revoking the current one behaves like logout. */
export const revokeSession = (id: string) =>
  apiFetch<{ status: string }>(`/me/sessions/${id}`, { method: "DELETE" })

/** Pre-flight check for a reset-password link. Lets the form surface
 *  an "expired" state before the user types a new password. `reason`
 *  distinguishes "invalid" (no such token — usually a stale link from a
 *  reset DB or a mangled URL), "used" (consumed, typically because the
 *  user requested a newer reset email after this one), "expired" (past
 *  TTL), or "wrong_purpose". Omitted when valid. */
export interface ResetTokenCheck {
  valid: boolean
  reason?: "invalid" | "used" | "wrong_purpose" | "expired"
}
export const verifyResetToken = (token: string) =>
  apiFetch<ResetTokenCheck>("/auth/verify-reset-token", {
    method: "POST",
    body: JSON.stringify({ token }),
  })

export const listDevices = () => apiFetch<PublicDevice[]>("/devices")

export const getDevice = (id: string) =>
  apiFetch<PublicDevice>(`/devices/${id}`)

export const createDevice = (body: {
  name: string
  os?: DeviceOs
  device_type?: DeviceType
  dns_override?: string[]
  /** Optional manual IPv4 — when set, the server reserves exactly this
   *  address. Omit to let the allocator pick the next free slot. */
  allocated_ip?: string
  /** Optional per-device monthly cap in bytes. Server clamps to the
   *  account cap and defaults to the user's own cap when omitted (so
   *  the device stays bounded by the account quota out of the box). */
  monthly_byte_cap?: number | null
}) =>
  apiFetch<CreatedDevice>("/devices", {
    method: "POST",
    body: JSON.stringify(body),
  })

/** Re-render the device's .conf from the server's stored private key.
 *  Returns the same shape as `createDevice` so the existing "device
 *  created" dialog UI can render it. The key is always stored, so this
 *  works for any non-revoked device. */
export const redownloadDeviceConf = (id: string) =>
  apiFetch<CreatedDevice>(`/devices/${id}/conf`)

export const patchDevice = (
  id: string,
  body: {
    name?: string
    os?: DeviceOs
    device_type?: DeviceType
    dns_override?: string[] | null
  }
) =>
  apiFetch<{ status: string }>(`/devices/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  })

export const deleteDevice = (id: string) =>
  apiFetch<{ status: string }>(`/devices/${id}`, { method: "DELETE" })

export const pauseDevice = (id: string) =>
  apiFetch<{ status: string }>(`/devices/${id}/pause`, { method: "POST" })

export const unpauseDevice = (id: string) =>
  apiFetch<{ status: string }>(`/devices/${id}/unpause`, { method: "POST" })

/** Regenerate the device's keypair server-side. Returns the same shape
 *  as `createDevice` — the device row, a freshly rendered wg-conf with
 *  the new private key, and a QR SVG. The old config stops working the
 *  moment this call returns; the user must re-import / re-scan. The new
 *  key is always stored (KEK-encrypted) for later re-download. */
export const rotateDeviceKeys = (id: string) =>
  apiFetch<CreatedDevice>(`/devices/${id}/rotate-keys`, {
    method: "POST",
  })

/** Persist the user's preferred device order. The server bulk-assigns
 *  `display_order` so the arrangement reflects on every signed-in
 *  session. `ids` must be the full set of the caller's device ids in
 *  the new order. */
export const reorderDevices = (ids: string[]) =>
  apiFetch<{ status: string; updated: number }>("/devices/order", {
    method: "PUT",
    body: JSON.stringify({ ids }),
  })

export const setDeviceDns = (id: string, dns_names: string[]) =>
  apiFetch<{ dns_names: string[] }>(`/devices/${id}/dns`, {
    method: "PUT",
    body: JSON.stringify({ dns_names }),
  })

/** User-facing per-device monthly cap setter. Pass `null` (or 0) to
 *  clear the device-level cap — the account cap still applies. The
 *  server clamps explicit values to the caller's own account cap. */
export const setMyDeviceQuota = (id: string, monthly_byte_cap: number | null) =>
  apiFetch<{ status: string }>(`/devices/${id}/quota`, {
    method: "PUT",
    body: JSON.stringify({ monthly_byte_cap }),
  })

/** A single entry in the device's activity timeline. `action` is a dotted
 *  identifier — see the action catalogue in DeviceTimeline. `metadata`
 *  is an opaque JSON blob shaped per-action. */
export interface DeviceEvent {
  id: number
  action: string
  metadata: Record<string, unknown>
  created_at: string
}

/**
 * Audit-log entries targeting a device, newest first. Supports keyset
 * pagination for infinite scroll: pass `beforeId` (the last row's id from the
 * previous page) to fetch the next, older page.
 */
export const listDeviceEvents = (
  id: string,
  opts: { limit?: number; beforeId?: number } = {}
) => {
  const params = new URLSearchParams()
  if (opts.limit != null) params.set("limit", String(opts.limit))
  if (opts.beforeId != null) params.set("before_id", String(opts.beforeId))
  const qs = params.toString()
  return apiFetch<DeviceEvent[]>(`/devices/${id}/events${qs ? `?${qs}` : ""}`)
}

/** Pre-flight DNS-name availability probe used by the create-device
 *  dialog. Returns whether the candidate FQDN matches the server regex
 *  and whether it's currently held by some other device. */
export interface DnsCheck {
  valid: boolean
  available: boolean
  reason?: "invalid" | "taken"
}
export const checkDnsName = (name: string) =>
  apiFetch<DnsCheck>(`/devices/dns-check?name=${encodeURIComponent(name)}`)

// --- raw tick-level history (server-side bandwidth_samples / server_samples) ---
// Used to hydrate the live charts on page load. The chart then continues
// from `Event::StatsDelta` / `Event::ServerSample` arriving over the WS.

export interface DeviceHistoryPoint {
  sampled_at: string
  rx_bytes: number
  tx_bytes: number
}

export interface DeviceHistoryResponse {
  device_id: string
  from: string
  to: string
  samples: DeviceHistoryPoint[]
}

export interface ServerHistoryPoint {
  sampled_at: string
  total_rx_bytes: number
  total_tx_bytes: number
  peer_count: number
  online_count: number
  handshake_count: number
}

export interface ServerHistoryResponse {
  server_id: string
  from: string
  to: string
  samples: ServerHistoryPoint[]
}

interface HistoryOpts {
  /** RFC3339 (`new Date(...).toISOString()`). Default: now - 5 min. */
  from?: string
  /** RFC3339. Default: now. */
  to?: string
  /** Row limit. Hard cap server-side: 10000. */
  limit?: number
}

function historyQs(opts?: HistoryOpts): string {
  const params = new URLSearchParams()
  if (opts?.from) params.set("from", opts.from)
  if (opts?.to) params.set("to", opts.to)
  if (opts?.limit !== undefined) params.set("limit", String(opts.limit))
  const s = params.toString()
  return s ? `?${s}` : ""
}

export const deviceHistory = (id: string, opts?: HistoryOpts) =>
  apiFetch<DeviceHistoryResponse>(`/devices/${id}/history${historyQs(opts)}`)

export const serverHistory = (id: string, opts?: HistoryOpts) =>
  apiFetch<ServerHistoryResponse>(`/servers/${id}/history${historyQs(opts)}`)

// --- OHLC bandwidth candles (trading-style HL + average) ---------------------
// The 1-minute candle is the source of truth (worker-accumulated); coarser
// timeframes are derived on read and the long ones (1d/7d/1mo) come from a
// daily rollup. Rates are bits/sec.

export type Timeframe =
  | "1m"
  | "3m"
  | "5m"
  | "15m"
  | "30m"
  | "1h"
  | "1d"
  | "7d"
  | "1mo"

export const TIMEFRAMES: { value: Timeframe; label: string }[] = [
  { value: "1m", label: "1 min" },
  { value: "3m", label: "3 min" },
  { value: "5m", label: "5 min" },
  { value: "15m", label: "15 min" },
  { value: "30m", label: "30 min" },
  { value: "1h", label: "1 hour" },
  { value: "1d", label: "1 day" },
  { value: "7d", label: "7 day" },
  { value: "1mo", label: "1 month" },
]

export interface Candle {
  bucket_start: string
  rx_high: number
  rx_low: number
  rx_avg: number
  tx_high: number
  tx_low: number
  tx_avg: number
}

export interface CandleResponse {
  tf: Timeframe
  candles: Candle[]
}

// `before` (RFC3339) pages backwards: returns only candles older than it, so
// the chart can lazily load history as the user pans/scrolls left.
function candleQs(tf: Timeframe, limit: number, before?: string): string {
  const p = new URLSearchParams({ tf, limit: String(limit) })
  if (before) p.set("before", before)
  return p.toString()
}

export const deviceCandles = (
  id: string,
  tf: Timeframe = "1m",
  limit = 120,
  before?: string
) =>
  apiFetch<CandleResponse>(
    `/devices/${id}/candles?${candleQs(tf, limit, before)}`
  )

export const serverCandles = (
  id: string,
  tf: Timeframe = "1m",
  limit = 120,
  before?: string
) =>
  apiFetch<CandleResponse>(
    `/servers/${id}/candles?${candleQs(tf, limit, before)}`
  )

/** User-aggregate candles across all of the caller's devices (dashboard). */
export const userCandles = (
  tf: Timeframe = "1m",
  limit = 120,
  before?: string
) => apiFetch<CandleResponse>(`/candles?${candleQs(tf, limit, before)}`)

/** Admin: per-device candles for any device — backs the live bandwidth chart
 *  on the admin device-detail page. */
export const adminDeviceCandles = (
  deviceId: string,
  tf: Timeframe = "1m",
  limit = 120,
  before?: string
) =>
  apiFetch<CandleResponse>(
    `/admin/devices/${deviceId}/candles?${candleQs(tf, limit, before)}`
  )

/** Admin: aggregate candles across all of a target user's devices — backs the
 *  live bandwidth chart on the admin user-detail page. */
export const adminUserCandles = (
  userId: string,
  tf: Timeframe = "1m",
  limit = 120,
  before?: string
) =>
  apiFetch<CandleResponse>(
    `/admin/users/${userId}/candles?${candleQs(tf, limit, before)}`
  )

// --- 2FA -----------------------------------------------------------------

export interface TotpSetupResponse {
  secret: string
  provisioning_uri: string
  qr_svg: string
}

export const totpSetup = () =>
  apiFetch<TotpSetupResponse>("/auth/totp/setup", { method: "POST" })

export const totpEnable = (secret: string, code: string) =>
  apiFetch<{ recovery_codes: string[] }>("/auth/totp/enable", {
    method: "POST",
    body: JSON.stringify({ secret, code }),
  })

export const totpDisable = (code: string) =>
  apiFetch<{ status: string }>("/auth/totp/disable", {
    method: "POST",
    body: JSON.stringify({ code }),
  })

/** Rotate recovery codes. Requires a current TOTP code; returns the fresh
 *  batch exactly once and invalidates every previous code. */
export const totpRegenerateRecovery = (code: string) =>
  apiFetch<{ recovery_codes: string[] }>("/auth/totp/recovery-codes", {
    method: "POST",
    body: JSON.stringify({ code }),
  })

// --- account -------------------------------------------------------------

export const deleteAccount = () =>
  apiFetch<{ status: string }>("/me/account", { method: "DELETE" })

// --- admin ---------------------------------------------------------------

export interface AdminUser {
  id: string
  email: string
  role: UserRole
  status: UserStatus
  totp_enabled: boolean
  created_at: string
  last_login_at: string | null
  device_count: number
}

export interface AdminUserListFilters {
  q?: string
  status?: UserStatus
  role?: UserRole
  totp_enabled?: boolean
}

function adminUserListQs(
  filters: AdminUserListFilters,
  limit?: number,
  offset?: number
): string {
  const params = new URLSearchParams()
  if (filters.q) params.set("q", filters.q)
  if (filters.status) params.set("status", filters.status)
  if (filters.role) params.set("role", filters.role)
  if (filters.totp_enabled !== undefined) {
    params.set("totp_enabled", String(filters.totp_enabled))
  }
  if (limit !== undefined) params.set("limit", String(limit))
  if (offset !== undefined) params.set("offset", String(offset))
  return params.toString()
}

export const adminListUsers = (
  filters: AdminUserListFilters = {},
  limit = 50,
  offset = 0
) =>
  apiFetch<{ total: number; items: AdminUser[] }>(
    `/admin/users?${adminUserListQs(filters, limit, offset)}`
  )

// ── User detail (admin) ──────────────────────────────────────────────
// Bundles core user fields, quota state, the device list, and recent
// audit entries that target this user. One request hydrates the whole
// admin user-detail page.

interface AdminUserDetail {
  id: string
  email: string
  role: UserRole
  status: UserStatus
  totp_enabled: boolean
  must_change_password: boolean
  created_at: string
  last_login_at: string | null
  email_verified_at: string | null
  password_changed_at: string
  current_month_bytes: number
  monthly_byte_cap: number | null
  quota_resets_at: string | null
  device_count: number
  /** Max active (non-revoked) devices this user may create (admin-settable). */
  device_limit: number
}

interface AdminUserDevice {
  id: string
  name: string
  os: DeviceOs
  status: DeviceStatus
  allocated_ip: string
  dns_names: string[]
  last_handshake_at: string | null
  /** Most recent `host:port` the peer connected from (Phase 2 / Stage A). */
  last_peer_endpoint: string | null
  /** Wall-clock when the current `last_peer_endpoint` was first seen. */
  last_peer_endpoint_at: string | null
  created_at: string
  /** Per-device monthly byte cap (null/0 = no device cap; account cap still
   *  applies). Enforced alongside the account cap by the quota sweep. */
  monthly_byte_cap: number | null
  /** The device's own bandwidth used this monthly cycle. */
  current_month_bytes: number
  /** True when the quota sweep auto-paused this device (device or account
   *  cap), vs. a manual user pause. */
  auto_paused: boolean
}

export interface EndpointHistoryRow {
  id: number
  endpoint: string
  observed_at: string
}

export const adminListDeviceEndpointHistory = (deviceId: string) =>
  apiFetch<EndpointHistoryRow[]>(`/admin/devices/${deviceId}/endpoint-history`)

/** One WireGuard connection session — see migration 18. `ended_at`
 *  is `null` while the session is still open. `rx_bytes_at_end` /
 *  `tx_bytes_at_end` are `null` for open sessions OR for sessions that
 *  were swept closed by the worker-startup pass (no clean offline
 *  observation, so the counters never got snapshot). */
export interface ConnectionSessionRow {
  id: number
  device_id: string
  user_id: string
  started_at: string
  ended_at: string | null
  peer_endpoint_at_start: string | null
  peer_endpoint_at_end: string | null
  rx_bytes_at_start: number
  tx_bytes_at_start: number
  rx_bytes_at_end: number | null
  tx_bytes_at_end: number | null
}

export const adminListDeviceConnectionHistory = (deviceId: string) =>
  apiFetch<ConnectionSessionRow[]>(
    `/admin/devices/${deviceId}/connection-history`
  )

// ── Admin device detail (Phase 2 / Stage B) ─────────────────────────

interface AdminDeviceOwner {
  id: string
  email: string
  role: UserRole
  status: UserStatus
}

interface AdminDeviceDetail {
  id: string
  user_id: string
  server_id: string
  name: string
  os: DeviceOs
  device_type: DeviceType
  status: DeviceStatus
  allocated_ip: string
  public_key: string
  dns_names: string[]
  dns_override: string[] | null
  last_handshake_at: string | null
  last_peer_endpoint: string | null
  last_peer_endpoint_at: string | null
  created_at: string
  /** Authoritative lifetime totals (same source as the user device card) so
   *  the admin RX/TX/Total KPIs match the owner's view. */
  total_rx_bytes: number
  total_tx_bytes: number
  /** Per-device monthly quota for the Quota KPI. */
  monthly_byte_cap: number | null
  current_month_bytes: number
  auto_paused: boolean
}

interface AdminDeviceActivity {
  id: number
  action: string
  metadata: unknown
  created_at: string
}

export interface AdminDeviceDetailResponse {
  device: AdminDeviceDetail
  owner: AdminDeviceOwner
  activity: AdminDeviceActivity[]
}

export const adminGetDeviceDetail = (id: string) =>
  apiFetch<AdminDeviceDetailResponse>(`/admin/devices/${id}`)

/** Admin moderation controls — pause / resume / revoke any device without
 *  impersonating its owner. Same WG + DNS side effects as the owner's own
 *  actions; audit-logged as `admin.device_*` with the owner in metadata. */
export const adminPauseDevice = (id: string) =>
  apiFetch<{ status: string }>(`/admin/devices/${id}/pause`, { method: "POST" })

export const adminUnpauseDevice = (id: string) =>
  apiFetch<{ status: string }>(`/admin/devices/${id}/unpause`, {
    method: "POST",
  })

export const adminRevokeDevice = (id: string) =>
  apiFetch<{ status: string }>(`/admin/devices/${id}`, { method: "DELETE" })

/** Referenced via `import("@/lib/api").AdminUserActivity` type position in
 * UserDetail's timeline builder — knip can't see those, hence the tag.
 * @public */
export interface AdminUserActivity {
  id: number
  action: string
  metadata: unknown
  /** Full client IP captured when the audit row was written. `null`
   *  for worker / CLI / state-transition emitters. */
  ip: string | null
  /** Raw `User-Agent` header. `null` for the same set as `ip`. */
  user_agent: string | null
  target_type: string | null
  target_id: string | null
  created_at: string
}

export interface AdminUserDetailResponse {
  user: AdminUserDetail
  devices: AdminUserDevice[]
  /** Audit entries where this user is either the actor (their own
   *  actions) or the target (admin actions taken on them). Newest
   *  first, capped at 100. */
  activity: AdminUserActivity[]
  /** Recent `session_events` rows scoped to this user. Newest first,
   *  capped at 50. */
  session_events: SessionEventRow[]
  /** Recent `connection_sessions` across all of the user's devices.
   *  Newest first, capped at 50. */
  connection_sessions: ConnectionSessionRow[]
}

export const adminGetUserDetail = (id: string) =>
  apiFetch<AdminUserDetailResponse>(`/admin/users/${id}`)

// ── Admin user overrides ────────────────────────────────────────────────

export const adminSetUserRole = (id: string, role: UserRole) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/role`, {
    method: "PUT",
    body: JSON.stringify({ role }),
  })

/** Email a password-reset link to the user. Reuses the same token
 *  machinery the public forgot-password flow uses, so the user follows
 *  the normal reset UI to set a new password. */
export const adminSendPasswordReset = (id: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/reset-password`, {
    method: "POST",
  })

/** Wipe the user's TOTP secret + recovery codes. Used for support
 *  recovery when a user has lost access to their authenticator. The user
 *  can re-enroll on the next sign-in. */
export const adminDisableUser2FA = (id: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/disable-2fa`, {
    method: "POST",
  })

/** Permanently delete the user and ALL their data: peers removed, and
 *  every row tied to the user (devices, sessions, logs, bandwidth,
 *  preferences, …) purged from the DB. Irreversible. */
export const adminDeleteUser = (id: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}`, {
    method: "DELETE",
  })

/** Force-logout — invalidate every open session for this user. The
 *  user is forced to sign in again on every device. Useful when a
 *  user reports their account is acting strangely or after rotating
 *  their password externally. */
export const adminRevokeUserSessions = (id: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/sessions/revoke-all`, {
    method: "POST",
  })

/** Admin override: change a user's email. Server normalises (trim +
 *  lowercase) and rejects collisions before writing. */
export const adminSetUserEmail = (id: string, email: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/email`, {
    method: "PUT",
    body: JSON.stringify({ email }),
  })

export interface AdminCreateUserBody {
  email: string
  role?: UserRole
}

export interface AdminCreatedUser {
  id: string
  email: string
  role: UserRole
  status: UserStatus
}

export const adminCreateUser = (body: AdminCreateUserBody) =>
  apiFetch<AdminCreatedUser>("/admin/users", {
    method: "POST",
    body: JSON.stringify(body),
  })

export interface AdminStats {
  total: number
  active: number
  suspended: number
  pending_verification: number
  devices_total: number
  /** Devices online now (active + handshake within 180s), DB-computed with
   *  the same rule as the user dashboard so it's correct on first paint. */
  online_now: number
}
/** Deployment-wide user + device counts. Use this for the admin KPI
 *  strip rather than summing client-side over a paginated list. */
export const adminStats = () => apiFetch<AdminStats>("/admin/stats")

export interface AdminFleetBandwidth {
  rx_bytes: number
  tx_bytes: number
}
export const adminFleetBandwidth = () =>
  apiFetch<AdminFleetBandwidth>("/admin/bandwidth")

export const adminSetUserStatus = (id: string, status: UserStatus) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/status`, {
    method: "PUT",
    body: JSON.stringify({ status }),
  })

export const adminSetUserQuota = (
  id: string,
  monthly_byte_cap: number | null
) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/quota`, {
    method: "PUT",
    body: JSON.stringify({ monthly_byte_cap }),
  })

/** Set a user's active-device cap (1–1000). */
export const adminSetUserDeviceLimit = (id: string, device_limit: number) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/device-limit`, {
    method: "PUT",
    body: JSON.stringify({ device_limit }),
  })

/** Set (or clear, with null) a single device's monthly byte cap. */
export const adminSetDeviceQuota = (
  deviceId: string,
  monthly_byte_cap: number | null
) =>
  apiFetch<{ status: string }>(`/admin/devices/${deviceId}/quota`, {
    method: "PUT",
    body: JSON.stringify({ monthly_byte_cap }),
  })

export interface AuditRow {
  id: number
  actor_user_id: string | null
  action: string
  target_type: string | null
  target_id: string | null
  metadata: unknown
  /** Full client IP serialised as an `IpNetwork` ("203.0.113.42/32").
   *  Captured for audit rows written from a route handler. `null` for
   *  worker/CLI-emitted rows. Renamed from `ip_prefix` in migration 20. */
  ip: string | null
  /** Raw `User-Agent` header from the request that produced this audit
   *  row. `null` for non-route-handler emitters. */
  user_agent: string | null
  created_at: string
}

export interface AdminAuditFilters {
  action?: string
  actor_user_id?: string
  target_id?: string
  target_type?: string
  /** RFC3339 lower bound (inclusive). */
  since?: string
  /** RFC3339 upper bound (exclusive). */
  until?: string
}

function adminAuditQs(
  filters: AdminAuditFilters,
  limit?: number,
  offset?: number
): string {
  const p = new URLSearchParams()
  if (filters.action) p.set("action", filters.action)
  if (filters.actor_user_id) p.set("actor_user_id", filters.actor_user_id)
  if (filters.target_id) p.set("target_id", filters.target_id)
  if (filters.target_type) p.set("target_type", filters.target_type)
  if (filters.since) p.set("since", filters.since)
  if (filters.until) p.set("until", filters.until)
  if (limit !== undefined) p.set("limit", String(limit))
  if (offset !== undefined) p.set("offset", String(offset))
  return p.toString()
}

export const adminListAudit = (
  filters: AdminAuditFilters = {},
  limit = 100,
  offset = 0
) =>
  apiFetch<{ total: number; items: AuditRow[] }>(
    `/admin/audit?${adminAuditQs(filters, limit, offset)}`
  )

export const adminAuditCsvUrl = (
  filters: AdminAuditFilters = {},
  limit = 5000
) => `/api/v1/admin/audit.csv?${adminAuditQs(filters, limit)}`

export interface FailedLoginRow {
  id: number
  email_attempted: string | null
  reason: string
  /** Full client IP serialised as an `IpNetwork` ("203.0.113.42/32").
   *  Renamed from `ip_prefix` in migration 20. */
  ip: string | null
  /** Raw User-Agent header from the failing request. */
  user_agent: string | null
  attempted_at: string
}

export const adminListFailedLogins = (limit = 100, offset = 0) =>
  apiFetch<{ total: number; items: FailedLoginRow[] }>(
    `/admin/failed-logins?limit=${limit}&offset=${offset}`
  )

// ── Session events (Phase 2 / Stage B) ──────────────────────────────────

export type SessionEventKind =
  | "login"
  | "logout"
  | "idle_timeout"
  | "suspicious_login"
  | "password_change"
  | "totp_enable"
  | "totp_disable"
  | "impersonation_start"
  | "impersonation_end"

export interface SessionEventRow {
  id: number
  user_id: string
  event: SessionEventKind
  /** Full client IP as `IpNetwork` ("203.0.113.42/32"). */
  ip: string | null
  user_agent: string | null
  /** Free-form event-specific context: `previous_ip` for suspicious_login,
   *  `via` for password_change ("settings" | "email_link"), `by_user_id` /
   *  `by_email` for impersonation_*. Always an object, never null. */
  metadata: Record<string, unknown>
  created_at: string
}

export interface AdminSessionEventFilters {
  user_id?: string
  event?: SessionEventKind
  ip?: string
  since?: string
  until?: string
}

function adminSessionEventsQs(
  f: AdminSessionEventFilters,
  limit: number,
  offset: number
): string {
  const params = new URLSearchParams()
  params.set("limit", String(limit))
  params.set("offset", String(offset))
  if (f.user_id) params.set("user_id", f.user_id)
  if (f.event) params.set("event", f.event)
  if (f.ip) params.set("ip", f.ip)
  if (f.since) params.set("since", f.since)
  if (f.until) params.set("until", f.until)
  return params.toString()
}

export const adminListSessionEvents = (
  filters: AdminSessionEventFilters = {},
  limit = 100,
  offset = 0
) =>
  apiFetch<{ total: number; items: SessionEventRow[] }>(
    `/admin/session-events?${adminSessionEventsQs(filters, limit, offset)}`
  )

// ── Access logs (Phase 2 / Stage B) ─────────────────────────────────────

export interface AccessLogRow {
  id: number
  created_at: string
  user_id: string | null
  method: string
  path: string
  status: number
  latency_ms: number
  /** Full client IP as `IpNetwork` ("203.0.113.42/32"). */
  ip: string | null
  user_agent: string | null
  request_id: string | null
}

export interface AdminAccessLogFilters {
  user_id?: string
  method?: string
  /** Path prefix — matches every row whose `path` starts with this
   *  string. SQL-side does the `LIKE 'foo%'` matching. */
  path?: string
  /** Lower / upper bounds on the HTTP status code (both inclusive). */
  status_min?: number
  status_max?: number
  ip?: string
  since?: string
  until?: string
}

function adminAccessLogsQs(
  f: AdminAccessLogFilters,
  limit: number,
  offset: number
): string {
  const params = new URLSearchParams()
  params.set("limit", String(limit))
  params.set("offset", String(offset))
  if (f.user_id) params.set("user_id", f.user_id)
  if (f.method) params.set("method", f.method)
  if (f.path) params.set("path", f.path)
  if (f.status_min != null) params.set("status_min", String(f.status_min))
  if (f.status_max != null) params.set("status_max", String(f.status_max))
  if (f.ip) params.set("ip", f.ip)
  if (f.since) params.set("since", f.since)
  if (f.until) params.set("until", f.until)
  return params.toString()
}

export const adminListAccessLogs = (
  filters: AdminAccessLogFilters = {},
  limit = 100,
  offset = 0
) =>
  apiFetch<{ total: number; items: AccessLogRow[] }>(
    `/admin/access-logs?${adminAccessLogsQs(filters, limit, offset)}`
  )

// ── Finder (Phase 2 / Stage B) ──────────────────────────────────────────

interface FinderCounts {
  audit_logs: number
  failed_logins: number
  session_events: number
  access_logs: number
  peer_endpoint_history: number
  connection_sessions: number
}

export interface FinderUserMatch {
  id: string
  email: string
  matched_on: "email" | "last_login_ip"
}

export interface FinderDeviceMatch {
  id: string
  user_id: string
  /** Owner's email — lets the finder group device matches by user. */
  user_email: string
  name: string
  allocated_ip: string
  last_peer_endpoint: string | null
  matched_on: "name" | "allocated_ip" | "last_peer_endpoint"
}

export interface FinderResponse {
  query: string
  /** Detected query kind. `"ip"` for a bare host address, `"endpoint"`
   *  for `host:port`, `"regex"` for a `/pattern/` POSIX regex,
   *  `"text"` otherwise. */
  kind: "ip" | "endpoint" | "regex" | "text"
  counts: FinderCounts
  users: FinderUserMatch[]
  devices: FinderDeviceMatch[]
}

export const adminFinder = (q: string) =>
  apiFetch<FinderResponse>(`/admin/finder?q=${encodeURIComponent(q)}`)

export interface MaintenanceState {
  maintenance_mode: boolean
  maintenance_message: string | null
  updated_at: string
}

export const adminGetMaintenance = () =>
  apiFetch<MaintenanceState>("/admin/maintenance")

export const adminSetMaintenance = (
  maintenance_mode: boolean,
  maintenance_message?: string | null
) =>
  apiFetch<{ status: string }>("/admin/maintenance", {
    method: "PUT",
    body: JSON.stringify({
      maintenance_mode,
      maintenance_message: maintenance_message ?? null,
    }),
  })

export const adminGetUserPolicy = () =>
  apiFetch<UserPolicySnapshot>("/admin/user-policy")

export const adminSetUserPolicy = (policy: UserPolicySnapshot) =>
  apiFetch<{ status: string }>("/admin/user-policy", {
    method: "PUT",
    body: JSON.stringify(policy),
  })

// ---------------------------------------------------------------------------
// Servers (admin)
// ---------------------------------------------------------------------------

export interface AdminServerRow {
  id: string
  name: string
  region: string
  endpoint_host: string
  endpoint_port: number
  public_key: string
  cidr: string
  dns_servers: string[]
  mtu: number
  is_active: boolean
  /** WireGuard PersistentKeepalive (seconds) handed to peers on this server.
   *  `0` disables keepalive. */
  persistent_keepalive: number
  /** Cumulative lifetime RX/TX across this server's devices (accurate
   *  device-lifetime sum). Powers the server-live card's RX/TX totals. */
  rx_total: number
  tx_total: number
}

export const adminListServers = () =>
  apiFetch<AdminServerRow[]>("/admin/servers")

export const adminPatchServer = (
  id: string,
  body: {
    endpoint_host?: string
    endpoint_port?: number
    mtu?: number
    dns_servers?: string[]
    persistent_keepalive?: number
  }
) =>
  apiFetch<{ status: string }>(`/admin/servers/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  })

export const adminRotateServerKeys = (id: string) =>
  apiFetch<{
    status: string
    new_public_key: string
    wg0_conf_rewritten: boolean
    warning: string
  }>(`/admin/servers/${id}/rotate-keys`, { method: "POST" })

// ── Admin: server detail + bandwidth history ──────────────────────────

interface AdminServerDeviceRow {
  id: string
  user_id: string
  user_email: string
  name: string
  os: DeviceOs
  status: DeviceStatus
  allocated_ip: string
  last_handshake_at: string | null
  created_at: string
}

export interface AdminServerDetail {
  server: AdminServerRow
  device_count_active: number
  device_count_paused: number
  device_count_total: number
  devices: AdminServerDeviceRow[]
}

export const adminGetServerDetail = (id: string) =>
  apiFetch<AdminServerDetail>(`/admin/servers/${id}`)

/** Admin-only: every non-revoked device across the fleet, each row
 *  carrying its owning `user_id`. Powers the admin topology view. */
export const adminListDevices = () => apiFetch<PublicDevice[]>("/admin/devices")

// ---------------------------------------------------------------------------
// Connections (live conntrack flows for the topology view)
// ---------------------------------------------------------------------------

/** One side of a flow. When the IP matches a known peer, `device_id` /
 *  `user_id` are populated and `name` is the device's display name; for
 *  foreign endpoints (the internet target, NAT gateway, …) `name` is
 *  `"External"` and the ids are absent. */
interface FlowEndpoint {
  ip: string
  name: string
  device_id?: string
  user_id?: string
}

/** One conntrack flow row. Ports are absent for connectionless protocols
 *  (icmp) or partially-parsed lines. Protocol is lower-case
 *  (`tcp` / `udp` / `icmp` / …). */
export interface Flow {
  source: FlowEndpoint
  target: FlowEndpoint
  source_port?: number
  target_port?: number
  protocol: string
}

/** Caller-scoped flows: only those where one endpoint is one of the
 *  caller's own devices. Used by the user-app topology page. */
export const listConnections = () => apiFetch<Flow[]>("/connections")

/** Admin-wide flows: every flow touching any known peer, plus foreign
 *  endpoints those peers talked to. Used by the admin topology page. */
export const adminListConnections = () => apiFetch<Flow[]>("/admin/connections")

export const adminImpersonateUser = (id: string) =>
  apiFetch<{ status: string }>(`/admin/users/${id}/impersonate`, {
    method: "POST",
  })

export const adminStopImpersonation = () =>
  apiFetch<{ status: string }>("/admin/impersonate/stop", { method: "POST" })
