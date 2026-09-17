import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconChevronDown,
  IconDeviceDesktop,
  IconLogout,
  IconMail,
  IconPlayerPause,
  IconPlayerPlay,
  IconShieldOff,
  IconTrash,
  IconUserShield,
  IconUserX,
} from "@tabler/icons-react"
import { useMemo, useState } from "react"
import { Link, useNavigate, useParams } from "react-router"
import { toast } from "sonner"

import { CandleChart } from "@/components/charts/CandleChart"
import { ConfirmDialog } from "@/components/ConfirmDialog"
import { Identicon } from "@/components/Identicon"
import { PageStagger, StaggerItem } from "@/components/motion"
import { RelativeTime } from "@/components/RelativeTime"
import { Kbd, PageHead, Panel, Pill, type PillTone } from "@/components/swiss"
import { StatusPill, type Status } from "@/components/StatusPill"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { Switch } from "@/components/ui/switch"
import {
  ApiError,
  type ConnectionSessionRow,
  type DeviceStatus,
  type EndpointHistoryRow,
  type UserRole,
  type UserStatus,
  adminDeleteUser,
  adminDisableUser2FA,
  adminGetUserDetail,
  adminImpersonateUser,
  adminListDeviceConnectionHistory,
  adminListDeviceEndpointHistory,
  adminPauseDevice,
  adminRevokeDevice,
  adminRevokeUserSessions,
  adminUnpauseDevice,
  adminSetDeviceQuota,
  adminSetUserDeviceLimit,
  adminSetUserEmail,
  adminSetUserQuota,
  adminSetUserRole,
  adminSetUserStatus,
  me as fetchMe,
} from "@/lib/api"
import { formatBytes } from "@/lib/units"
import { useAuth } from "@/stores/auth"

const USER_STATUS_TO_PILL: Record<UserStatus, Status> = {
  active: "active",
  suspended: "revoked",
  pending_verification: "pending",
  deleted: "offline",
}

const DEVICE_STATUS_TO_PILL: Record<DeviceStatus, Status> = {
  active: "active",
  paused: "paused",
  revoked: "revoked",
}

const GIB = 1024 ** 3

// Activity timeline: how many of the merged entries the inline panel shows
// before the "View all" side sheet takes over with the full list.
const ACTIVITY_PREVIEW_COUNT = 10

export function UserDetailPage() {
  const { id = "" } = useParams<{ id: string }>()
  const self = useAuth((s) => s.user)
  const setUser = useAuth((s) => s.setUser)
  const navigate = useNavigate()
  const qc = useQueryClient()

  const [suspendOpen, setSuspendOpen] = useState(false)
  const [impersonateOpen, setImpersonateOpen] = useState(false)
  const [quotaOpen, setQuotaOpen] = useState(false)
  const [deviceLimitOpen, setDeviceLimitOpen] = useState(false)
  const [disable2faOpen, setDisable2faOpen] = useState(false)
  const [deleteOpen, setDeleteOpen] = useState(false)
  const [roleOpen, setRoleOpen] = useState(false)
  const [revokeSessionsOpen, setRevokeSessionsOpen] = useState(false)
  const [editEmailOpen, setEditEmailOpen] = useState(false)
  // Device whose per-device quota dialog is open (null = closed).
  const [quotaDevice, setQuotaDevice] = useState<{
    id: string
    name: string
    cap: number | null
  } | null>(null)
  // ID of the device whose endpoint-history dialog is open; null means
  // closed. Phase 2 / Stage A — admins click the "history" cell on any
  // device row to drill into every distinct WG endpoint that peer has
  // ever connected from.
  const [endpointDevice, setEndpointDevice] = useState<{
    id: string
    name: string
  } | null>(null)
  // Phase 2 / Stage B — connection-session history per device.
  const [connectionDevice, setConnectionDevice] = useState<{
    id: string
    name: string
  } | null>(null)
  // Full activity timeline in a side sheet (inline panel shows a preview).
  const [activityOpen, setActivityOpen] = useState(false)

  const detailQ = useQuery({
    queryKey: ["admin", "user", id],
    queryFn: () => adminGetUserDetail(id),
    enabled: !!id,
    // Keep quota usage, device status, and handshakes fresh without a manual
    // refresh. Live device/user events also invalidate this key (see
    // LiveStatsProvider) for instant status flips; this is the steady tick that
    // advances the ever-growing usage counters between events.
    refetchInterval: 10_000,
  })

  const u = detailQ.data?.user
  const isSelf = u?.id === self?.id

  // Merged activity timeline (audit + session events + connection sessions),
  // newest-first. Computed once so the inline preview and the full side sheet
  // share the same list.
  const timeline = useMemo(
    () => (detailQ.data ? buildTimeline(detailQ.data) : []),
    [detailQ.data]
  )
  const deviceNameById = useMemo(
    () =>
      new Map(
        (detailQ.data?.devices ?? []).map((d) => [d.id, d.name] as const)
      ),
    [detailQ.data]
  )

  const invalidateUser = () => {
    void qc.invalidateQueries({ queryKey: ["admin", "user", id] })
    void qc.invalidateQueries({ queryKey: ["admin", "users"] })
  }

  // ── Device moderation (same endpoints as the admin device detail page) ──
  const [revokeDevice, setRevokeDevice] = useState<{
    id: string
    name: string
  } | null>(null)
  const devicePauseM = useMutation({
    mutationFn: (d: { id: string; paused: boolean }) =>
      d.paused ? adminUnpauseDevice(d.id) : adminPauseDevice(d.id),
    onSuccess: (_, d) => {
      invalidateUser()
      toast.success(d.paused ? "Device resumed" : "Device paused")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })
  const deviceRevokeM = useMutation({
    mutationFn: (deviceId: string) => adminRevokeDevice(deviceId),
    onSuccess: () => {
      invalidateUser()
      setRevokeDevice(null)
      toast.success("Device revoked — IP released and peer removed")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const setStatusM = useMutation({
    mutationFn: (status: UserStatus) => adminSetUserStatus(id, status),
    onSuccess: () => {
      invalidateUser()
      setSuspendOpen(false)
      toast.success("User status updated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const setRoleM = useMutation({
    mutationFn: (role: UserRole) => adminSetUserRole(id, role),
    onSuccess: (_, role) => {
      invalidateUser()
      setRoleOpen(false)
      toast.success(`Role set to ${role}`)
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const disable2faM = useMutation({
    mutationFn: () => adminDisableUser2FA(id),
    onSuccess: () => {
      invalidateUser()
      setDisable2faOpen(false)
      toast.success("2FA disabled — user can re-enroll on next sign-in")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const revokeSessionsM = useMutation({
    mutationFn: () => adminRevokeUserSessions(id),
    onSuccess: () => {
      setRevokeSessionsOpen(false)
      toast.success("All sessions for this user invalidated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const setEmailM = useMutation({
    mutationFn: (email: string) => adminSetUserEmail(id, email),
    onSuccess: () => {
      invalidateUser()
      setEditEmailOpen(false)
      toast.success("Email updated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const deleteM = useMutation({
    mutationFn: () => adminDeleteUser(id),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["admin", "users"] })
      setDeleteOpen(false)
      toast.success("User deleted")
      void navigate("/admin/users")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const impersonateM = useMutation({
    mutationFn: () => adminImpersonateUser(id),
    onSuccess: async () => {
      try {
        const updated = await fetchMe()
        setUser(updated)
        setImpersonateOpen(false)
        toast.success(`Now acting as ${u?.email ?? "user"}`)
        void navigate("/app")
      } catch {
        toast.error("Impersonation started but failed to refresh session")
      }
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow={`Admin · User · ${id.slice(0, 8)}`}
          title={
            u ? (
              <span className="inline-flex items-center gap-3">
                <span className="flex size-9 shrink-0 items-center justify-center border border-border bg-card p-0.5">
                  <Identicon seed={u.email} size={32} cells={5} />
                </span>
                <span className="truncate">{u.email}</span>
              </span>
            ) : detailQ.isLoading ? (
              "Loading…"
            ) : (
              "Unknown user"
            )
          }
          sub={
            u && (
              <span className="flex flex-wrap items-center gap-2">
                <Pill
                  tone={u.role === "admin" ? "info" : "neutral"}
                  dot={false}
                >
                  {u.role}
                </Pill>
                <StatusPill
                  status={USER_STATUS_TO_PILL[u.status] ?? "pending"}
                  label={u.status.replace(/_/g, " ")}
                />
                {u.totp_enabled ? (
                  <Pill tone="ok" dot={false}>
                    2FA on
                  </Pill>
                ) : (
                  <Pill tone="warn" dot={false}>
                    2FA off
                  </Pill>
                )}
                {u.must_change_password && (
                  <Pill tone="warn" dot={false}>
                    must change password
                  </Pill>
                )}
                {isSelf && (
                  <span className="font-mono text-[10px] text-muted-foreground/70 uppercase">
                    you
                  </span>
                )}
              </span>
            )
          }
          right={
            u && !isSelf ? (
              <>
                {u.status === "active" && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setImpersonateOpen(true)}
                  >
                    <IconUserX className="size-4" />
                    Impersonate
                  </Button>
                )}
                {u.status === "active" ? (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setSuspendOpen(true)}
                  >
                    Suspend
                  </Button>
                ) : u.status === "suspended" ? (
                  <Button size="sm" onClick={() => setStatusM.mutate("active")}>
                    Unsuspend
                  </Button>
                ) : null}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button size="sm" variant="outline">
                      Actions
                      <IconChevronDown className="size-3.5" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="min-w-[14rem]">
                    <DropdownMenuLabel>Identity</DropdownMenuLabel>
                    <DropdownMenuItem onSelect={() => setEditEmailOpen(true)}>
                      <IconMail />
                      Edit email
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      onSelect={() => setDisable2faOpen(true)}
                      disabled={!u.totp_enabled}
                    >
                      <IconShieldOff />
                      Disable 2FA
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onSelect={() => setRevokeSessionsOpen(true)}
                    >
                      <IconLogout />
                      Force-logout all sessions
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuLabel>Role</DropdownMenuLabel>
                    <DropdownMenuItem onSelect={() => setRoleOpen(true)}>
                      <IconUserShield />
                      {u.role === "admin"
                        ? "Demote to user"
                        : "Promote to admin"}
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      variant="destructive"
                      onSelect={() => setDeleteOpen(true)}
                    >
                      <IconTrash />
                      Delete user…
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null
          }
        />
      </StaggerItem>

      {detailQ.isLoading && (
        <StaggerItem>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            <Skeleton className="h-40 rounded-none" />
            <Skeleton className="h-40 rounded-none" />
          </div>
        </StaggerItem>
      )}

      {u && (
        <>
          <StaggerItem>
            <div className="grid min-w-0 grid-cols-1 gap-3 md:grid-cols-2">
              <Panel
                title="Account"
                right={
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setDeviceLimitOpen(true)}
                  >
                    Device limit
                  </Button>
                }
              >
                <KvList
                  items={[
                    [
                      "ID",
                      <code key="id" className="font-mono text-[11px]">
                        {u.id}
                      </code>,
                    ],
                    [
                      "Created",
                      <RelativeTime
                        key="c"
                        value={u.created_at}
                        fallback="—"
                      />,
                    ],
                    [
                      "Last login",
                      <RelativeTime
                        key="l"
                        value={u.last_login_at}
                        fallback="Never"
                      />,
                    ],
                    [
                      "Email verified",
                      u.email_verified_at ? (
                        <RelativeTime
                          key="v"
                          value={u.email_verified_at}
                          fallback="—"
                        />
                      ) : (
                        <span className="text-amber-600 dark:text-amber-400">
                          Not verified
                        </span>
                      ),
                    ],
                    [
                      "Password changed",
                      <RelativeTime
                        key="p"
                        value={u.password_changed_at}
                        fallback="—"
                      />,
                    ],
                    ["Devices", `${u.device_count} / ${u.device_limit}`],
                  ]}
                />
              </Panel>

              <Panel
                title="Bandwidth quota"
                right={
                  // Shown for self too — admins may edit their own cap (the
                  // API has no self-restriction here, unlike suspend/delete).
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setQuotaOpen(true)}
                  >
                    Edit cap
                  </Button>
                }
              >
                <QuotaSummary
                  used={u.current_month_bytes}
                  cap={u.monthly_byte_cap}
                  resetsAt={u.quota_resets_at}
                />
              </Panel>
            </div>
          </StaggerItem>

          <StaggerItem>
            <CandleChart
              scope="admin-user"
              id={id}
              height={300}
              title="Bandwidth"
              sub="Live bandwidth across all of this user's devices — drag to pan, scroll to zoom"
            />
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Devices"
              sub={`${detailQ.data?.devices.length ?? 0} active or paused`}
              flush
            >
              {detailQ.data && detailQ.data.devices.length > 0 ? (
                <div className="zv-table-scroll">
                  <table className="zv-table">
                    <thead>
                      <tr>
                        <th>Name</th>
                        <th>OS</th>
                        <th>IP</th>
                        <th>Status</th>
                        <th>Quota</th>
                        <th>Last handshake</th>
                        <th>Last endpoint</th>
                        <th>Connections</th>
                        <th>Created</th>
                        <th className="w-20">Actions</th>
                      </tr>
                    </thead>
                    <tbody>
                      {detailQ.data.devices.map((d) => (
                        <tr key={d.id}>
                          <td>
                            <Link
                              to={`/admin/devices/${d.id}`}
                              className="inline-flex items-center gap-2 underline-offset-2 hover:text-foreground hover:underline"
                              title="Open admin device detail"
                            >
                              <IconDeviceDesktop className="size-4 text-muted-foreground" />
                              <span className="font-medium">{d.name}</span>
                            </Link>
                          </td>
                          <td className="text-muted-foreground capitalize">
                            {d.os}
                          </td>
                          <td className="font-mono text-xs">
                            {d.allocated_ip}
                          </td>
                          <td>
                            <div className="flex flex-col gap-0.5">
                              <StatusPill
                                status={
                                  DEVICE_STATUS_TO_PILL[d.status] ?? "offline"
                                }
                                label={d.status}
                              />
                              {d.auto_paused && (
                                <span
                                  className="font-mono text-[10px] text-amber-600 dark:text-amber-400"
                                  title="Auto-paused by the quota sweep; resumes on reset"
                                >
                                  quota
                                </span>
                              )}
                            </div>
                          </td>
                          <td>
                            <DeviceQuotaCell
                              used={d.current_month_bytes}
                              cap={d.monthly_byte_cap}
                              onEdit={() =>
                                setQuotaDevice({
                                  id: d.id,
                                  name: d.name,
                                  cap: d.monthly_byte_cap,
                                })
                              }
                            />
                          </td>
                          <td className="font-mono text-xs text-muted-foreground">
                            <RelativeTime
                              value={d.last_handshake_at}
                              fallback="Never"
                            />
                          </td>
                          <td className="font-mono text-xs">
                            {d.last_peer_endpoint ? (
                              <button
                                type="button"
                                onClick={() =>
                                  setEndpointDevice({ id: d.id, name: d.name })
                                }
                                className="text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                                title="View full endpoint history"
                              >
                                {d.last_peer_endpoint}
                              </button>
                            ) : (
                              <span className="text-muted-foreground">—</span>
                            )}
                          </td>
                          <td className="font-mono text-xs text-muted-foreground">
                            <button
                              type="button"
                              onClick={() =>
                                setConnectionDevice({ id: d.id, name: d.name })
                              }
                              className="underline-offset-2 hover:text-foreground hover:underline"
                              title="View connection-session history"
                            >
                              history
                            </button>
                          </td>
                          <td className="font-mono text-xs text-muted-foreground">
                            <RelativeTime value={d.created_at} fallback="—" />
                          </td>
                          <td>
                            {d.status !== "revoked" && (
                              <div className="flex items-center gap-1">
                                <button
                                  type="button"
                                  disabled={devicePauseM.isPending}
                                  onClick={() =>
                                    devicePauseM.mutate({
                                      id: d.id,
                                      paused: d.status === "paused",
                                    })
                                  }
                                  className="p-1 text-muted-foreground hover:text-foreground disabled:opacity-50"
                                  title={
                                    d.status === "paused"
                                      ? "Resume device"
                                      : "Pause device"
                                  }
                                >
                                  {d.status === "paused" ? (
                                    <IconPlayerPlay className="size-4" />
                                  ) : (
                                    <IconPlayerPause className="size-4" />
                                  )}
                                </button>
                                <button
                                  type="button"
                                  onClick={() =>
                                    setRevokeDevice({ id: d.id, name: d.name })
                                  }
                                  className="p-1 text-muted-foreground hover:text-destructive"
                                  title="Revoke device"
                                >
                                  <IconTrash className="size-4" />
                                </button>
                              </div>
                            )}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                  No devices.
                </div>
              )}
            </Panel>
          </StaggerItem>

          <StaggerItem>
            <Panel
              title="Activity timeline"
              sub={
                timeline.length > ACTIVITY_PREVIEW_COUNT
                  ? `Showing ${ACTIVITY_PREVIEW_COUNT} of ${timeline.length} — audit + session + connection events, newest first`
                  : "Audit + session events + connection sessions, merged chronologically — newest first"
              }
              flush
              right={
                timeline.length > ACTIVITY_PREVIEW_COUNT ? (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setActivityOpen(true)}
                  >
                    View all ({timeline.length})
                  </Button>
                ) : undefined
              }
            >
              {timeline.length === 0 ? (
                <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                  No activity yet.
                </div>
              ) : (
                <TimelineTable
                  items={timeline.slice(0, ACTIVITY_PREVIEW_COUNT)}
                  deviceNameById={deviceNameById}
                />
              )}
            </Panel>
          </StaggerItem>
        </>
      )}

      {/* Full activity timeline — opened by "View all". */}
      <Sheet open={activityOpen} onOpenChange={setActivityOpen}>
        <SheetContent
          side="right"
          className="flex flex-col gap-0 p-0 data-[side=right]:w-full data-[side=right]:sm:max-w-3xl"
        >
          <SheetHeader className="border-b border-border p-4">
            <SheetTitle>Activity timeline</SheetTitle>
            <SheetDescription>
              Audit + session events + connection sessions, merged
              chronologically — newest first ({timeline.length})
            </SheetDescription>
          </SheetHeader>
          <div className="min-h-0 flex-1 overflow-auto">
            {timeline.length === 0 ? (
              <div className="py-8 text-center font-mono text-sm text-muted-foreground">
                No activity yet.
              </div>
            ) : (
              <TimelineTable
                items={timeline}
                deviceNameById={deviceNameById}
                full
              />
            )}
          </div>
        </SheetContent>
      </Sheet>

      {detailQ.isError && (
        <StaggerItem>
          <Panel title="Failed to load">
            <p className="text-sm text-muted-foreground">
              {detailQ.error instanceof ApiError
                ? detailQ.error.message
                : "Could not fetch user."}
            </p>
          </Panel>
        </StaggerItem>
      )}

      <ConfirmDialog
        open={revokeDevice !== null}
        onOpenChange={(o) => {
          if (!o) setRevokeDevice(null)
        }}
        title={`Revoke ${revokeDevice?.name ?? "device"}?`}
        description="Permanently revokes the device: the WG peer is removed and its IP is released for reallocation. The user keeps their account and other devices."
        confirmLabel="Revoke device"
        destructive
        pending={deviceRevokeM.isPending}
        onConfirm={() => {
          if (revokeDevice) deviceRevokeM.mutate(revokeDevice.id)
        }}
      />
      <ConfirmDialog
        open={suspendOpen}
        onOpenChange={setSuspendOpen}
        title={`Suspend ${u?.email ?? "user"}?`}
        description="Suspends the account, blocks future logins, and keeps device rows in place. Reversible — Unsuspend brings them back."
        confirmLabel="Suspend"
        destructive
        pending={setStatusM.isPending}
        onConfirm={() => setStatusM.mutate("suspended")}
      />

      <ConfirmDialog
        open={impersonateOpen}
        onOpenChange={setImpersonateOpen}
        title={`Impersonate ${u?.email ?? "user"}?`}
        description="You will be redirected to the dashboard acting as this user. A banner will remind you to exit impersonation when done."
        confirmLabel="Impersonate"
        pending={impersonateM.isPending}
        onConfirm={() => impersonateM.mutate()}
      />

      <ConfirmDialog
        open={disable2faOpen}
        onOpenChange={setDisable2faOpen}
        title={`Disable 2FA for ${u?.email ?? "user"}?`}
        description="Wipes the user's TOTP secret and recovery codes. Use this when a user has lost their authenticator. They can re-enroll 2FA from Settings → Security after they sign in."
        confirmLabel="Disable 2FA"
        destructive
        pending={disable2faM.isPending}
        onConfirm={() => disable2faM.mutate()}
      />

      <ConfirmDialog
        open={revokeSessionsOpen}
        onOpenChange={setRevokeSessionsOpen}
        title={`Force-logout ${u?.email ?? "user"}?`}
        description="Invalidates every open session for this user. They'll need to sign in again on every device. The action is logged."
        confirmLabel="Force logout"
        destructive
        pending={revokeSessionsM.isPending}
        onConfirm={() => revokeSessionsM.mutate()}
      />

      {u && (
        <EditEmailDialog
          open={editEmailOpen}
          onOpenChange={setEditEmailOpen}
          currentEmail={u.email}
          pending={setEmailM.isPending}
          onSubmit={(email) => setEmailM.mutate(email)}
        />
      )}

      <ConfirmDialog
        open={roleOpen}
        onOpenChange={setRoleOpen}
        title={
          u?.role === "admin"
            ? `Demote ${u?.email ?? "user"} to user?`
            : `Promote ${u?.email ?? "user"} to admin?`
        }
        description={
          u?.role === "admin"
            ? "Removes admin privileges. They keep their account and devices."
            : "Grants full administrative access — including the ability to manage other users, servers, and impersonation."
        }
        confirmLabel={u?.role === "admin" ? "Demote" : "Promote"}
        destructive={u?.role === "admin"}
        pending={setRoleM.isPending}
        onConfirm={() =>
          setRoleM.mutate(u?.role === "admin" ? "user" : "admin")
        }
      />

      {u && (
        <ConfirmDialog
          open={deleteOpen}
          onOpenChange={setDeleteOpen}
          title={`Delete ${u.email}?`}
          description="Permanently deletes this user and ALL of their data — every device/peer is removed and all sessions, logs, bandwidth history, and preferences are purged from the database. This is irreversible."
          confirmLabel="Delete permanently"
          destructive
          confirmText={u.email}
          pending={deleteM.isPending}
          onConfirm={() => deleteM.mutate()}
        />
      )}

      {u && (
        <QuotaDialog
          open={quotaOpen}
          onOpenChange={setQuotaOpen}
          userId={u.id}
          currentCap={u.monthly_byte_cap}
          onSaved={invalidateUser}
        />
      )}

      {u && (
        <DeviceLimitDialog
          open={deviceLimitOpen}
          onOpenChange={setDeviceLimitOpen}
          userId={u.id}
          currentLimit={u.device_limit}
          deviceCount={u.device_count}
          onSaved={invalidateUser}
        />
      )}

      <DeviceQuotaDialog
        device={quotaDevice}
        onOpenChange={(open) => {
          if (!open) setQuotaDevice(null)
        }}
        onSaved={invalidateUser}
      />

      <EndpointHistoryDialog
        device={endpointDevice}
        onOpenChange={(open) => {
          if (!open) setEndpointDevice(null)
        }}
      />

      <ConnectionHistoryDialog
        device={connectionDevice}
        onOpenChange={(open) => {
          if (!open) setConnectionDevice(null)
        }}
      />
    </PageStagger>
  )
}

// ──────────────────────────────────────────────────────────────────────

function KvList({ items }: { items: [string, React.ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
      {items.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="font-mono text-xs tracking-wide text-muted-foreground uppercase">
            {k}
          </dt>
          <dd className="min-w-0 truncate text-foreground">{v}</dd>
        </div>
      ))}
    </dl>
  )
}

function QuotaSummary({
  used,
  cap,
  resetsAt,
}: {
  used: number
  cap: number | null
  resetsAt: string | null
}) {
  if (!cap || cap <= 0) {
    return (
      <div className="flex flex-col gap-2">
        <div className="font-mono text-2xl">{formatBytes(used)}</div>
        <div className="text-xs text-muted-foreground">
          No cap — unlimited usage this month
        </div>
        {resetsAt && (
          <div className="font-mono text-[11px] text-muted-foreground/70">
            Counter resets <RelativeTime value={resetsAt} fallback="—" />
          </div>
        )}
      </div>
    )
  }
  const pct = Math.min(100, Math.round((used / cap) * 100))
  const tone =
    pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-500" : "bg-emerald-500"
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between">
        <span className="font-mono text-2xl">{formatBytes(used)}</span>
        <span className="font-mono text-xs text-muted-foreground">
          / {formatBytes(cap)} ({pct}%)
        </span>
      </div>
      <div className="h-1.5 w-full overflow-hidden bg-muted">
        <div className={`h-full ${tone}`} style={{ width: `${pct}%` }} />
      </div>
      {resetsAt && (
        <div className="font-mono text-[11px] text-muted-foreground/70">
          Counter resets <RelativeTime value={resetsAt} fallback="—" />
        </div>
      )}
    </div>
  )
}

function QuotaDialog({
  open,
  onOpenChange,
  userId,
  currentCap,
  onSaved,
}: {
  open: boolean
  onOpenChange: (o: boolean) => void
  userId: string
  currentCap: number | null
  onSaved: () => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        {/* Mounted per open (Radix unmounts closed content), so the form
            seeds fresh from `currentCap` via useState initializers. */}
        {open && (
          <QuotaCapForm
            userId={userId}
            currentCap={currentCap}
            onClose={() => onOpenChange(false)}
            onSaved={onSaved}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function QuotaCapForm({
  userId,
  currentCap,
  onClose,
  onSaved,
}: {
  userId: string
  currentCap: number | null
  onClose: () => void
  onSaved: () => void
}) {
  // A switch decides whether the account is capped at all: off = unlimited,
  // on = a required positive GiB value (same model as the device dialogs).
  const [capEnabled, setCapEnabled] = useState(!!currentCap && currentCap > 0)
  const [gibStr, setGibStr] = useState(
    currentCap ? (currentCap / GIB).toFixed(2) : ""
  )

  const m = useMutation({
    mutationFn: (cap: number | null) => adminSetUserQuota(userId, cap),
    onSuccess: () => {
      toast.success("Quota updated")
      onSaved()
      onClose()
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const gib = Number(gibStr.trim())
  const capValid = gibStr.trim().length > 0 && Number.isFinite(gib) && gib > 0
  const canSave = !m.isPending && (!capEnabled || capValid)

  const submit = () => {
    m.mutate(capEnabled ? Math.round(gib * GIB) : null)
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>Edit monthly bandwidth cap</DialogTitle>
        <DialogDescription>
          Cap is enforced per calendar month. Toggle off for unlimited.
        </DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <Label htmlFor="cap-gib">Monthly cap (GiB)</Label>
          <Switch
            checked={capEnabled}
            onCheckedChange={setCapEnabled}
            aria-label="Enable a monthly bandwidth cap for this account"
          />
        </div>
        {capEnabled ? (
          <>
            <Input
              id="cap-gib"
              type="number"
              inputMode="decimal"
              step="0.5"
              min="0"
              value={gibStr}
              onChange={(e) => setGibStr(e.target.value)}
              placeholder="e.g. 100"
            />
            <p className="text-xs text-muted-foreground">
              All of the user's devices pause when the account hits this cap.
              {!capValid && (
                <span className="ml-1 text-destructive">
                  Enter a positive number of GiB.
                </span>
              )}
            </p>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">
            Unlimited — this account has no monthly bandwidth cap.
          </p>
        )}
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={!canSave} onClick={submit}>
          {m.isPending ? "Saving…" : "Save"}
        </Button>
      </DialogFooter>
    </>
  )
}

/** Admin control for a user's active-device (peer) cap. Mirrors
 *  [`QuotaDialog`] — mounts the form per open so it seeds from `currentLimit`. */
function DeviceLimitDialog({
  open,
  onOpenChange,
  userId,
  currentLimit,
  deviceCount,
  onSaved,
}: {
  open: boolean
  onOpenChange: (o: boolean) => void
  userId: string
  currentLimit: number
  deviceCount: number
  onSaved: () => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        {open && (
          <DeviceLimitForm
            userId={userId}
            currentLimit={currentLimit}
            deviceCount={deviceCount}
            onClose={() => onOpenChange(false)}
            onSaved={onSaved}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function DeviceLimitForm({
  userId,
  currentLimit,
  deviceCount,
  onClose,
  onSaved,
}: {
  userId: string
  currentLimit: number
  deviceCount: number
  onClose: () => void
  onSaved: () => void
}) {
  const [limitStr, setLimitStr] = useState(String(currentLimit))

  const m = useMutation({
    mutationFn: (limit: number) => adminSetUserDeviceLimit(userId, limit),
    onSuccess: () => {
      toast.success("Device limit updated")
      onSaved()
      onClose()
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const limit = Number(limitStr.trim())
  const valid =
    limitStr.trim().length > 0 &&
    Number.isInteger(limit) &&
    limit >= 1 &&
    limit <= 1000
  // Not an error — lowering below the current count is allowed; they just
  // can't add more until back under the cap.
  const belowCount = valid && limit < deviceCount

  return (
    <>
      <DialogHeader>
        <DialogTitle>Edit device limit</DialogTitle>
        <DialogDescription>
          Maximum active peers this user can create (1–1000). New peers are
          blocked once they reach it; existing peers are never removed.
        </DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-2">
        <Label htmlFor="device-limit">Max devices</Label>
        <Input
          id="device-limit"
          type="number"
          inputMode="numeric"
          step="1"
          min="1"
          max="1000"
          value={limitStr}
          onChange={(e) => setLimitStr(e.target.value)}
          placeholder="e.g. 10"
        />
        <p className="text-xs text-muted-foreground">
          Currently using {deviceCount}.
          {!valid && (
            <span className="ml-1 text-destructive">
              Enter a whole number from 1 to 1000.
            </span>
          )}
          {belowCount && (
            <span className="ml-1 text-amber-600 dark:text-amber-400">
              Below the {deviceCount} they already have — those stay, but they
              can't add more until under the new limit.
            </span>
          )}
        </p>
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={m.isPending || !valid} onClick={() => m.mutate(limit)}>
          {m.isPending ? "Saving…" : "Save"}
        </Button>
      </DialogFooter>
    </>
  )
}

/** Compact per-device quota readout for the devices table: a used/cap line
 *  over a color-tiered mini bar, or a "Set cap" affordance when uncapped.
 *  Clicking either opens the per-device cap editor. */
function DeviceQuotaCell({
  used,
  cap,
  onEdit,
}: {
  used: number
  cap: number | null
  onEdit: () => void
}) {
  if (!cap || cap <= 0) {
    return (
      <button
        type="button"
        onClick={onEdit}
        className="font-mono text-[11px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
        title="Set a per-device monthly cap"
      >
        {formatBytes(used)} · set cap
      </button>
    )
  }
  const pct = Math.min(100, Math.round((used / cap) * 100))
  const tone =
    pct >= 90 ? "bg-red-500" : pct >= 70 ? "bg-amber-500" : "bg-emerald-500"
  return (
    <button
      type="button"
      onClick={onEdit}
      className="group flex w-[120px] flex-col gap-1 text-left"
      title="Edit per-device monthly cap"
    >
      <span className="font-mono text-[11px] text-muted-foreground group-hover:text-foreground">
        {formatBytes(used)} / {formatBytes(cap)} ({pct}%)
      </span>
      <span className="block h-1 w-full overflow-hidden bg-muted">
        <span className={`block h-full ${tone}`} style={{ width: `${pct}%` }} />
      </span>
    </button>
  )
}

/** Per-device cap editor — same GiB-in/bytes-out contract as the account
 *  [`QuotaDialog`], targeting a single device. */
function DeviceQuotaDialog({
  device,
  onOpenChange,
  onSaved,
}: {
  device: { id: string; name: string; cap: number | null } | null
  onOpenChange: (o: boolean) => void
  onSaved: () => void
}) {
  return (
    <Dialog open={!!device} onOpenChange={onOpenChange}>
      <DialogContent>
        {/* Keyed per device + mounted per open, so the form seeds fresh
            from the device's cap via useState initializers. */}
        {device && (
          <DeviceCapForm
            key={device.id}
            device={device}
            onClose={() => onOpenChange(false)}
            onSaved={onSaved}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function DeviceCapForm({
  device,
  onClose,
  onSaved,
}: {
  device: { id: string; name: string; cap: number | null }
  onClose: () => void
  onSaved: () => void
}) {
  // Same switch model as the account-cap dialog: off = no device cap,
  // on = a required positive GiB value.
  const [capEnabled, setCapEnabled] = useState(!!device.cap && device.cap > 0)
  const [gibStr, setGibStr] = useState(
    device.cap ? (device.cap / GIB).toFixed(2) : ""
  )

  const m = useMutation({
    mutationFn: (cap: number | null) => adminSetDeviceQuota(device.id, cap),
    onSuccess: () => {
      toast.success("Device quota updated")
      onSaved()
      onClose()
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const gib = Number(gibStr.trim())
  const capValid = gibStr.trim().length > 0 && Number.isFinite(gib) && gib > 0
  const canSave = !m.isPending && (!capEnabled || capValid)

  const submit = () => {
    m.mutate(capEnabled ? Math.round(gib * GIB) : null)
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>Per-device data cap</DialogTitle>
        <DialogDescription>
          Cap for <span className="font-medium">{device.name}</span>, enforced
          per calendar month. The device pauses when it hits this cap or the
          account cap — whichever comes first. Toggle off to remove the device
          cap.
        </DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <Label htmlFor="dev-cap-gib">Monthly cap (GiB)</Label>
          <Switch
            checked={capEnabled}
            onCheckedChange={setCapEnabled}
            aria-label="Enable a monthly data cap for this device"
          />
        </div>
        {capEnabled ? (
          <>
            <Input
              id="dev-cap-gib"
              type="number"
              inputMode="decimal"
              step="0.5"
              min="0"
              value={gibStr}
              onChange={(e) => setGibStr(e.target.value)}
              placeholder="e.g. 100"
            />
            <p className="text-xs text-muted-foreground">
              The device auto-pauses when it reaches this cap.
              {!capValid && (
                <span className="ml-1 text-destructive">
                  Enter a positive number of GiB.
                </span>
              )}
            </p>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">
            No device cap — the account cap still applies.
          </p>
        )}
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button disabled={!canSave} onClick={submit}>
          {m.isPending ? "Saving…" : "Save"}
        </Button>
      </DialogFooter>
    </>
  )
}

function toneForAction(action: string): PillTone {
  if (action.startsWith("admin.impersonate")) return "warn"
  if (action.includes("suspend") || action.includes("delete")) return "err"
  if (action.includes("status")) return "info"
  if (action.includes("quota")) return "info"
  if (action.includes("role")) return "info"
  if (action.includes("password_reset")) return "warn"
  if (action.includes("2fa")) return "warn"
  return "neutral"
}

/**
 * Inline dialog for the admin "Edit email" action. Validates client-side
 * for shape + change + length, then defers to the server for uniqueness
 * (which is checked atomically against the CITEXT column).
 */
function EditEmailDialog({
  open,
  onOpenChange,
  currentEmail,
  pending,
  onSubmit,
}: {
  open: boolean
  onOpenChange: (o: boolean) => void
  currentEmail: string
  pending: boolean
  onSubmit: (email: string) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {/* Keyed on `open` so each open mounts a fresh body seeded from the
          current email — no effect-driven state syncing. */}
      <EditEmailDialogBody
        key={String(open)}
        onOpenChange={onOpenChange}
        currentEmail={currentEmail}
        pending={pending}
        onSubmit={onSubmit}
      />
    </Dialog>
  )
}

function EditEmailDialogBody({
  onOpenChange,
  currentEmail,
  pending,
  onSubmit,
}: {
  onOpenChange: (o: boolean) => void
  currentEmail: string
  pending: boolean
  onSubmit: (email: string) => void
}) {
  const [value, setValue] = useState(currentEmail)

  const trimmed = value.trim().toLowerCase()
  const sameAsCurrent = trimmed === currentEmail.toLowerCase()
  const looksValid = trimmed.includes("@") && trimmed.length >= 3
  const canSubmit = !pending && looksValid && !sameAsCurrent

  return (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Edit email</DialogTitle>
        <DialogDescription>
          Updates the address of record for{" "}
          <code className="font-mono">{currentEmail}</code>. The user keeps
          their devices, role, password, and 2FA. Their existing sessions are
          not invalidated automatically — use "Force-logout all sessions" if you
          want that.
        </DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-2">
        <Label htmlFor="edit-email">New email</Label>
        <Input
          id="edit-email"
          type="email"
          inputMode="email"
          autoComplete="off"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="user@example.com"
        />
        {sameAsCurrent && trimmed.length > 0 && (
          <p className="text-[11px] text-muted-foreground">
            Same as current — nothing to change.
          </p>
        )}
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={() => onOpenChange(false)}>
          Cancel
        </Button>
        <Button disabled={!canSubmit} onClick={() => onSubmit(trimmed)}>
          {pending ? "Saving…" : "Save"}
        </Button>
      </DialogFooter>
    </DialogContent>
  )
}

function summarizeMetadata(metadata: unknown): string {
  if (!metadata || typeof metadata !== "object") return ""
  const obj = metadata as Record<string, unknown>
  const parts: string[] = []
  for (const [k, v] of Object.entries(obj)) {
    if (v == null) continue
    if (typeof v === "object") continue
    parts.push(`${k}=${String(v)}`)
    if (parts.length >= 3) break
  }
  return parts.join(" · ")
}

// ──────────────────────────────────────────────────────────────────────
// Phase 2 / Stage B — unified activity timeline. Merges three feeds
// fetched server-side (audit / session_events / connection_sessions)
// into one chronological list. Each item is a discriminated union and
// renders to one table row.

type TimelineItem =
  | {
      kind: "audit"
      id: number
      ts: string
      action: string
      metadata: unknown
      ip: string | null
      user_agent: string | null
      target_type: string | null
      target_id: string | null
    }
  | {
      kind: "session"
      id: number
      ts: string
      event: string
      ip: string | null
      user_agent: string | null
      metadata: unknown
    }
  | {
      kind: "connection"
      id: number
      ts: string
      device_id: string
      started_at: string
      ended_at: string | null
      endpoint_start: string | null
      endpoint_end: string | null
      rx: number | null
      tx: number | null
    }

function buildTimeline(d: {
  activity: import("@/lib/api").AdminUserActivity[]
  session_events: import("@/lib/api").SessionEventRow[]
  connection_sessions: import("@/lib/api").ConnectionSessionRow[]
}): TimelineItem[] {
  const items: TimelineItem[] = []
  for (const a of d.activity) {
    items.push({
      kind: "audit",
      id: a.id,
      ts: a.created_at,
      action: a.action,
      metadata: a.metadata,
      ip: a.ip,
      user_agent: a.user_agent,
      target_type: a.target_type,
      target_id: a.target_id,
    })
  }
  for (const e of d.session_events) {
    items.push({
      kind: "session",
      id: e.id,
      ts: e.created_at,
      event: e.event,
      ip: e.ip,
      user_agent: e.user_agent,
      metadata: e.metadata,
    })
  }
  for (const c of d.connection_sessions) {
    const rx =
      c.rx_bytes_at_end != null
        ? Math.max(0, c.rx_bytes_at_end - c.rx_bytes_at_start)
        : null
    const tx =
      c.tx_bytes_at_end != null
        ? Math.max(0, c.tx_bytes_at_end - c.tx_bytes_at_start)
        : null
    items.push({
      kind: "connection",
      id: c.id,
      ts: c.started_at,
      device_id: c.device_id,
      started_at: c.started_at,
      ended_at: c.ended_at,
      endpoint_start: c.peer_endpoint_at_start,
      endpoint_end: c.peer_endpoint_at_end,
      rx,
      tx,
    })
  }
  items.sort((a, b) => (a.ts < b.ts ? 1 : a.ts > b.ts ? -1 : 0))
  return items
}

/** The merged-timeline table — shared by the inline preview (sliced) and the
 *  full side sheet (every row). Columns auto-size to content (no `table-fixed`)
 *  with long cells capped + truncated, so nothing overlaps; any residual width
 *  scrolls horizontally. `full` (sheet) renders just the table and lets the
 *  parent own scrolling, so every row is reachable. */
function TimelineTable({
  items,
  deviceNameById,
  full = false,
}: {
  items: TimelineItem[]
  deviceNameById: Map<string, string>
  full?: boolean
}) {
  const table = (
    <table className="zv-table">
      <thead>
        <tr>
          <th className="w-[120px]">When</th>
          <th className="w-[90px]">Kind</th>
          <th>Event</th>
          <th className="w-[150px]">IP</th>
          <th>Detail</th>
        </tr>
      </thead>
      <tbody>
        {items.map((it) => (
          <TimelineRow
            key={`${it.kind}-${it.id}`}
            item={it}
            deviceNameById={deviceNameById}
          />
        ))}
      </tbody>
    </table>
  )
  if (full) return table
  return <div className="zv-table-scroll max-w-full min-w-0">{table}</div>
}

function TimelineRow({
  item,
  deviceNameById,
}: {
  item: TimelineItem
  deviceNameById: Map<string, string>
}) {
  if (item.kind === "audit") {
    const targetLabel = item.target_type
      ? `${item.target_type}${item.target_id ? `·${item.target_id.slice(0, 6)}` : ""}`
      : null
    return (
      <tr>
        <td className="font-mono text-xs text-muted-foreground">
          <RelativeTime value={item.ts} fallback="—" />
        </td>
        <td>
          <Pill tone="info" dot={false}>
            audit
          </Pill>
        </td>
        <td>
          <Pill tone={toneForAction(item.action)} dot={false}>
            {item.action}
          </Pill>
        </td>
        <td className="font-mono text-xs tabular-nums">
          {item.ip ? (
            item.ip.replace(/\/(32|128)$/, "")
          ) : (
            <span className="text-muted-foreground">—</span>
          )}
        </td>
        <td className="min-w-0 font-mono text-[11px] text-muted-foreground">
          <span
            className="block max-w-[440px] truncate"
            title={summarizeMetadata(item.metadata)}
          >
            {targetLabel && (
              <span className="mr-2 text-foreground">→ {targetLabel}</span>
            )}
            {summarizeMetadata(item.metadata)}
          </span>
        </td>
      </tr>
    )
  }
  if (item.kind === "session") {
    return (
      <tr>
        <td className="font-mono text-xs text-muted-foreground">
          <RelativeTime value={item.ts} fallback="—" />
        </td>
        <td>
          <Pill tone="ok" dot={false}>
            session
          </Pill>
        </td>
        <td>
          <Kbd>{item.event}</Kbd>
        </td>
        <td className="font-mono text-xs tabular-nums">
          {item.ip ? (
            item.ip.replace(/\/(32|128)$/, "")
          ) : (
            <span className="text-muted-foreground">—</span>
          )}
        </td>
        <td
          className="min-w-0 font-mono text-[11px] text-muted-foreground"
          title={item.user_agent ?? undefined}
        >
          <span className="block max-w-[440px] truncate">
            {summarizeMetadata(item.metadata)}
            {item.user_agent && (
              <span className="ml-2 inline-block max-w-full truncate align-bottom">
                {item.user_agent}
              </span>
            )}
          </span>
        </td>
      </tr>
    )
  }
  // connection
  const open = item.ended_at == null
  const deviceName =
    deviceNameById.get(item.device_id) ?? item.device_id.slice(0, 8)
  const duration = open
    ? "active"
    : connectionDuration(item.started_at, item.ended_at!)
  const endpoint = item.endpoint_start
    ? item.endpoint_start +
      (item.endpoint_end && item.endpoint_end !== item.endpoint_start
        ? ` → ${item.endpoint_end}`
        : "")
    : null
  return (
    <tr>
      <td className="font-mono text-xs text-muted-foreground">
        <RelativeTime value={item.ts} fallback="—" />
      </td>
      <td>
        <Pill tone={open ? "ok" : "neutral"} dot={open}>
          connection
        </Pill>
      </td>
      <td className="min-w-0">
        <span
          className="block max-w-[260px] truncate font-mono text-xs"
          title={`${deviceName} · ${open ? "active" : `closed after ${duration}`}`}
        >
          <span className="text-foreground">{deviceName}</span>
          <span className="text-muted-foreground"> · </span>
          {open ? "active" : `closed after ${duration}`}
        </span>
      </td>
      <td className="min-w-0 font-mono text-xs" title={endpoint ?? undefined}>
        {endpoint ? (
          <span className="block max-w-[220px] truncate">{endpoint}</span>
        ) : (
          <span className="text-muted-foreground">—</span>
        )}
      </td>
      <td className="font-mono text-[11px] text-muted-foreground tabular-nums">
        {item.rx != null && item.tx != null
          ? `↓ ${formatBytes(item.rx)} · ↑ ${formatBytes(item.tx)}`
          : open
            ? "in flight"
            : "—"}
      </td>
    </tr>
  )
}

function connectionDuration(startIso: string, endIso: string): string {
  const ms = new Date(endIso).getTime() - new Date(startIso).getTime()
  if (!Number.isFinite(ms) || ms < 0) return "—"
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ${(s % 60).toString().padStart(2, "0")}s`
  const h = Math.floor(m / 60)
  return `${h}h ${(m % 60).toString().padStart(2, "0")}m`
}

// ──────────────────────────────────────────────────────────────────────
// Phase 2 / Stage A — endpoint history dialog. Renders every distinct
// `host:port` the WG poller has ever observed for the device, newest
// first. Capped at 200 rows server-side; if a device sees more than
// that the dialog shows a footnote.

function EndpointHistoryDialog({
  device,
  onOpenChange,
}: {
  device: { id: string; name: string } | null
  onOpenChange: (open: boolean) => void
}) {
  const open = device != null
  const q = useQuery({
    queryKey: ["admin", "device", device?.id, "endpoint-history"],
    queryFn: () => adminListDeviceEndpointHistory(device!.id),
    enabled: open,
    staleTime: 30_000,
  })
  const rows: EndpointHistoryRow[] = q.data ?? []
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            Endpoint history{device ? ` · ${device.name}` : ""}
          </DialogTitle>
          <DialogDescription>
            Every distinct WireGuard peer endpoint observed for this device,
            newest first. Each row is the moment the endpoint changed against
            the previous observation — repeated polls of the same endpoint don't
            duplicate.
          </DialogDescription>
        </DialogHeader>
        <div className="max-h-[420px] overflow-y-auto">
          {q.isLoading && (
            <div className="py-8 text-center font-mono text-xs text-muted-foreground">
              Loading…
            </div>
          )}
          {q.isError && (
            <div className="py-8 text-center font-mono text-xs text-destructive">
              Failed to load history.
            </div>
          )}
          {q.data && rows.length === 0 && (
            <div className="py-8 text-center font-mono text-sm text-muted-foreground">
              No endpoints captured yet. The device hasn't completed a handshake
              since this feature shipped.
            </div>
          )}
          {q.data && rows.length > 0 && (
            <div className="zv-table-scroll">
              <table className="zv-table">
                <thead>
                  <tr>
                    <th>Endpoint</th>
                    <th className="w-[200px]">First seen</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r) => (
                    <tr key={r.id}>
                      <td className="font-mono text-xs">{r.endpoint}</td>
                      <td className="font-mono text-xs text-muted-foreground">
                        <RelativeTime value={r.observed_at} />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
        {rows.length === 200 && (
          <p className="font-mono text-[11px] text-muted-foreground">
            Showing the 200 most recent observations. Older entries exist but
            aren't surfaced here yet.
          </p>
        )}
      </DialogContent>
    </Dialog>
  )
}

// ──────────────────────────────────────────────────────────────────────
// Phase 2 / Stage B — connection-session history. One row per WG
// connection (online → offline pair) with duration, start/end
// endpoint, and rx/tx delivered. Open sessions show "active" and a
// `—` duration. Sessions closed by the worker-startup sweep (no clean
// offline observation) show their start byte counters but the end
// columns are blank.

function ConnectionHistoryDialog({
  device,
  onOpenChange,
}: {
  device: { id: string; name: string } | null
  onOpenChange: (open: boolean) => void
}) {
  const open = device != null
  const q = useQuery({
    queryKey: ["admin", "device", device?.id, "connection-history"],
    queryFn: () => adminListDeviceConnectionHistory(device!.id),
    enabled: open,
    staleTime: 30_000,
  })
  const rows: ConnectionSessionRow[] = q.data ?? []
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            Connection history{device ? ` · ${device.name}` : ""}
          </DialogTitle>
          <DialogDescription>
            One row per WireGuard connection. `RX` / `TX` are the bytes
            delivered during that session (snapshot end − snapshot start). Open
            sessions show a `—` for end-state columns until the peer
            disconnects.
          </DialogDescription>
        </DialogHeader>
        <div className="max-h-[420px] overflow-y-auto">
          {q.isLoading && (
            <div className="py-8 text-center font-mono text-xs text-muted-foreground">
              Loading…
            </div>
          )}
          {q.isError && (
            <div className="py-8 text-center font-mono text-xs text-destructive">
              Failed to load history.
            </div>
          )}
          {q.data && rows.length === 0 && (
            <div className="py-8 text-center font-mono text-sm text-muted-foreground">
              No connection sessions yet. The device hasn't transitioned online
              since this feature shipped.
            </div>
          )}
          {q.data && rows.length > 0 && (
            <div className="zv-table-scroll">
              <table className="zv-table">
                <thead>
                  <tr>
                    <th className="w-[180px]">Started</th>
                    <th className="w-[110px]">Duration</th>
                    <th>Endpoint</th>
                    <th className="text-right">RX</th>
                    <th className="text-right">TX</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((s) => {
                    const open = s.ended_at == null
                    const duration = open
                      ? "active"
                      : formatDuration(s.started_at, s.ended_at!)
                    const rx =
                      s.rx_bytes_at_end != null
                        ? Math.max(0, s.rx_bytes_at_end - s.rx_bytes_at_start)
                        : null
                    const tx =
                      s.tx_bytes_at_end != null
                        ? Math.max(0, s.tx_bytes_at_end - s.tx_bytes_at_start)
                        : null
                    const endpointChanged =
                      !open &&
                      s.peer_endpoint_at_end != null &&
                      s.peer_endpoint_at_end !== s.peer_endpoint_at_start
                    return (
                      <tr key={s.id}>
                        <td className="font-mono text-xs text-muted-foreground">
                          <RelativeTime value={s.started_at} />
                        </td>
                        <td className="font-mono text-xs tabular-nums">
                          {open ? (
                            <Pill tone="ok" dot>
                              active
                            </Pill>
                          ) : (
                            duration
                          )}
                        </td>
                        <td className="font-mono text-xs">
                          {s.peer_endpoint_at_start ?? (
                            <span className="text-muted-foreground">—</span>
                          )}
                          {endpointChanged && (
                            <span
                              className="text-muted-foreground"
                              title={`Ended on ${s.peer_endpoint_at_end}`}
                            >
                              {" → "}
                              {s.peer_endpoint_at_end}
                            </span>
                          )}
                        </td>
                        <td className="text-right font-mono text-xs text-muted-foreground tabular-nums">
                          {rx != null ? (
                            formatBytes(rx)
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </td>
                        <td className="text-right font-mono text-xs text-muted-foreground tabular-nums">
                          {tx != null ? (
                            formatBytes(tx)
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
        {rows.length === 200 && (
          <p className="font-mono text-[11px] text-muted-foreground">
            Showing the 200 most recent sessions. Older entries exist but aren't
            surfaced here yet.
          </p>
        )}
      </DialogContent>
    </Dialog>
  )
}

/** Compact duration formatter for the connection-history table.
 *  Shows seconds for <1 min, m:ss for <1 h, otherwise H:MM. */
function formatDuration(startIso: string, endIso: string): string {
  const ms = new Date(endIso).getTime() - new Date(startIso).getTime()
  if (!Number.isFinite(ms) || ms < 0) return "—"
  const totalSec = Math.floor(ms / 1000)
  if (totalSec < 60) return `${totalSec}s`
  const totalMin = Math.floor(totalSec / 60)
  if (totalMin < 60) {
    const s = totalSec % 60
    return `${totalMin}m ${s.toString().padStart(2, "0")}s`
  }
  const h = Math.floor(totalMin / 60)
  const m = totalMin % 60
  return `${h}h ${m.toString().padStart(2, "0")}m`
}
