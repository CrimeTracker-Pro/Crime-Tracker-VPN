import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconDotsVertical,
  IconCopy,
  IconEye,
  IconLogin2,
  IconLogout,
  IconPlus,
  IconSearch,
  IconShieldOff,
  IconTrash,
  IconUserCheck,
  IconUserShield,
  IconUserX,
  IconX,
} from "@tabler/icons-react"
import { useMemo, useState } from "react"

import { useResettingPage } from "@/hooks/useResettingPage"
import { Link, useNavigate } from "react-router"
import { toast } from "sonner"

import { ConfirmDialog } from "@/components/ConfirmDialog"
import { Identicon } from "@/components/Identicon"
import { PageStagger, StaggerItem } from "@/components/motion"
import { Pagination } from "@/components/Pagination"
import { RelativeTime } from "@/components/RelativeTime"
import { PageHead, Panel, Pill } from "@/components/swiss"
import { StatusPill, type Status } from "@/components/StatusPill"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Skeleton } from "@/components/ui/skeleton"
import { Switch } from "@/components/ui/switch"
import {
  ApiError,
  type AdminCreateUserBody,
  type AdminUser,
  type AdminUserListFilters,
  type UserPolicySnapshot,
  type UserRole,
  type UserStatus,
  adminCreateUser,
  adminListInvitations,
  adminCreateInvitationLink,
  adminResendInvitation,
  adminRevokeInvitation,
  adminDeleteUser,
  adminDisableUser2FA,
  adminGetUserPolicy,
  adminImpersonateUser,
  adminListUsers,
  adminRevokeUserSessions,
  adminSetUserPolicy,
  adminSetUserRole,
  adminSetUserStatus,
  me,
} from "@/lib/api"
import { useAuth } from "@/stores/auth"

const USER_STATUS_TO_PILL: Record<UserStatus, Status> = {
  active: "active",
  suspended: "revoked",
  pending_verification: "pending",
  deleted: "offline",
}

type StatusFilter = "all" | UserStatus
type RoleFilter = "all" | UserRole
type TotpFilter = "all" | "on" | "off"

export function UsersPage() {
  const self = useAuth((s) => s.user)
  const setUser = useAuth((s) => s.setUser)
  const qc = useQueryClient()
  const navigate = useNavigate()

  const [search, setSearch] = useState("")
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all")
  const [roleFilter, setRoleFilter] = useState<RoleFilter>("all")
  const [totpFilter, setTotpFilter] = useState<TotpFilter>("all")
  const [suspendTarget, setSuspendTarget] = useState<AdminUser | null>(null)
  const [impersonateTarget, setImpersonateTarget] = useState<AdminUser | null>(
    null
  )
  // Row-kebab actions that need a confirm step (security-sensitive or
  // destructive). Reversible actions (reset email, force-logout) fire directly.
  const [confirmAction, setConfirmAction] = useState<{
    user: AdminUser
    kind: "delete" | "promote" | "demote" | "disable2fa"
  } | null>(null)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [pageSize, setPageSize] = useState(50)
  // Page 0 whenever the search term, filters, or page size change —
  // a narrower filter (or wider page) can leave us pointing past the end of
  // the result set.
  const [page, setPage] = useResettingPage(
    JSON.stringify([search, statusFilter, roleFilter, totpFilter, pageSize])
  )
  // Bulk-selection state. Keeps ids only — re-derives the displayed
  // checkbox state from the current page's items, so navigating pages
  // preserves selection across visits.
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [bulkSuspendOpen, setBulkSuspendOpen] = useState(false)
  const [bulkDeleteOpen, setBulkDeleteOpen] = useState(false)

  const filters = useMemo<AdminUserListFilters>(
    () => ({
      q: search || undefined,
      status: statusFilter === "all" ? undefined : statusFilter,
      role: roleFilter === "all" ? undefined : roleFilter,
      totp_enabled: totpFilter === "all" ? undefined : totpFilter === "on",
    }),
    [search, statusFilter, roleFilter, totpFilter]
  )

  const usersQ = useQuery({
    queryKey: ["admin", "users", filters, page, pageSize],
    queryFn: () => adminListUsers(filters, pageSize, page * pageSize),
    placeholderData: (prev) => prev,
  })
  const invitationsQ = useQuery({
    queryKey: ["admin", "invitations"],
    queryFn: adminListInvitations,
  })
  const resendInviteM = useMutation({
    mutationFn: adminResendInvitation,
    onSuccess: () => { void qc.invalidateQueries({ queryKey: ["admin", "invitations"] }); toast.success("Invitation resent") },
    onError: (e: unknown) => { if (e instanceof ApiError) toast.error(e.message) },
  })
  const copyInviteLinkM = useMutation({
    mutationFn: adminCreateInvitationLink,
    onSuccess: async (data) => {
      await navigator.clipboard.writeText(data.link)
      void qc.invalidateQueries({ queryKey: ["admin", "invitations"] })
      toast.success("Fresh invitation link copied")
    },
    onError: (e: unknown) => { if (e instanceof ApiError) toast.error(e.message) },
  })
  const revokeInviteM = useMutation({
    mutationFn: adminRevokeInvitation,
    onSuccess: () => { void qc.invalidateQueries({ queryKey: ["admin", "invitations"] }); toast.success("Invitation revoked") },
    onError: (e: unknown) => { if (e instanceof ApiError) toast.error(e.message) },
  })

  const setStatusM = useMutation({
    mutationFn: ({ id, status }: { id: string; status: UserStatus }) =>
      adminSetUserStatus(id, status),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["admin", "users"] })
      setSuspendTarget(null)
      toast.success("User status updated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const impersonateM = useMutation({
    mutationFn: (id: string) => adminImpersonateUser(id),
    onSuccess: async () => {
      try {
        const updated = await me()
        setUser(updated)
        setImpersonateTarget(null)
        toast.success(`Now acting as ${impersonateTarget?.email ?? "user"}`)
        void navigate("/app")
      } catch {
        toast.error("Impersonation started but failed to refresh session")
      }
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  // ── Row-kebab mutations ────────────────────────────────────────────────
  const invalidateUsers = () =>
    void qc.invalidateQueries({ queryKey: ["admin", "users"] })
  const onActionError = (e: unknown) => {
    if (e instanceof ApiError) toast.error(e.message)
  }

  const revokeSessionsM = useMutation({
    mutationFn: (id: string) => adminRevokeUserSessions(id),
    onSuccess: () => toast.success("All sessions for this user invalidated"),
    onError: onActionError,
  })
  const disable2faM = useMutation({
    mutationFn: (id: string) => adminDisableUser2FA(id),
    onSuccess: () => {
      invalidateUsers()
      setConfirmAction(null)
      toast.success("2FA disabled — user can re-enroll on next sign-in")
    },
    onError: onActionError,
  })
  const setRoleM = useMutation({
    mutationFn: ({ id, role }: { id: string; role: UserRole }) =>
      adminSetUserRole(id, role),
    onSuccess: (_, { role }) => {
      invalidateUsers()
      setConfirmAction(null)
      toast.success(`Role set to ${role}`)
    },
    onError: onActionError,
  })
  const deleteM = useMutation({
    mutationFn: (id: string) => adminDeleteUser(id),
    onSuccess: () => {
      invalidateUsers()
      setConfirmAction(null)
      toast.success("User deleted")
    },
    onError: onActionError,
  })

  const items = useMemo(() => usersQ.data?.items ?? [], [usersQ.data])
  const filtersActive =
    statusFilter !== "all" ||
    roleFilter !== "all" ||
    totpFilter !== "all" ||
    !!search

  // Bulk-action helpers. Always exclude self from selection so an admin
  // can't accidentally include themselves in a bulk delete (the backend
  // rejects per-call anyway, but we don't want misleading partial-success
  // toasts).
  const selectableIds = useMemo(
    () => items.filter((u) => u.id !== self?.id).map((u) => u.id),
    [items, self?.id]
  )
  const allOnPageSelected =
    selectableIds.length > 0 && selectableIds.every((id) => selectedIds.has(id))
  const someOnPageSelected =
    selectableIds.some((id) => selectedIds.has(id)) && !allOnPageSelected
  const togglePageSelection = (on: boolean) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (on) selectableIds.forEach((id) => next.add(id))
      else selectableIds.forEach((id) => next.delete(id))
      return next
    })
  }
  const toggleRowSelection = (id: string, on: boolean) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (on) next.add(id)
      else next.delete(id)
      return next
    })
  }
  const clearSelection = () => setSelectedIds(new Set())

  // Bulk Suspend / Delete: fan out per-id with Promise.allSettled so a
  // single failure doesn't abort the whole batch. Aggregate the result
  // into one toast so the user sees "12 of 14 succeeded" rather than 14
  // individual notifications.
  const bulkSuspendM = useMutation({
    mutationFn: async (ids: string[]) => {
      const results = await Promise.allSettled(
        ids.map((id) => adminSetUserStatus(id, "suspended"))
      )
      return results
    },
    onSuccess: (results, ids) => {
      const ok = results.filter((r) => r.status === "fulfilled").length
      void qc.invalidateQueries({ queryKey: ["admin", "users"] })
      setBulkSuspendOpen(false)
      clearSelection()
      if (ok === ids.length) {
        toast.success(`Suspended ${ok} user${ok === 1 ? "" : "s"}`)
      } else {
        toast.warning(
          `Suspended ${ok} of ${ids.length} — the rest failed (likely already non-active or admin-protected)`
        )
      }
    },
  })
  const bulkDeleteM = useMutation({
    mutationFn: async (ids: string[]) => {
      const results = await Promise.allSettled(
        ids.map((id) => adminDeleteUser(id))
      )
      return results
    },
    onSuccess: (results, ids) => {
      const ok = results.filter((r) => r.status === "fulfilled").length
      void qc.invalidateQueries({ queryKey: ["admin", "users"] })
      setBulkDeleteOpen(false)
      clearSelection()
      if (ok === ids.length) {
        toast.success(`Deleted ${ok} user${ok === 1 ? "" : "s"}`)
      } else {
        toast.warning(
          `Deleted ${ok} of ${ids.length} — the rest failed (likely the last admin or self)`
        )
      }
    },
  })

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Admin · 02"
          title="Users"
          sub={`${usersQ.data?.total ?? 0} total · ${items.filter((u) => u.status === "active").length} active`}
          right={
            <Button
              size="sm"
              onClick={() => setInviteOpen(true)}
              className="gap-1.5"
            >
              <IconPlus className="size-4" />
              Invite user
            </Button>
          }
        />
      </StaggerItem>

      <StaggerItem>
        <Panel>
          <h2 className="mb-3 font-medium">Pending invitations</h2>
          {invitationsQ.data?.length ? invitationsQ.data.map((invite) => (
            <div key={invite.user_id} className="flex flex-wrap items-center justify-between gap-2 border-t border-border py-2 text-sm">
              <div><span>{invite.email}</span><span className="ml-2 text-muted-foreground">{new Date(invite.expires_at) < new Date() ? "Expired" : invite.verified_at ? "Email verified — awaiting Google sign-in" : "Awaiting email verification"}</span></div>
              <div className="flex gap-2">
                <Button size="sm" variant="outline" disabled={resendInviteM.isPending} onClick={() => resendInviteM.mutate(invite.user_id)}>Resend</Button>
                <Button size="sm" variant="outline" disabled={copyInviteLinkM.isPending} onClick={() => copyInviteLinkM.mutate(invite.user_id)}><IconCopy className="size-3.5" />Copy link</Button>
                <Button size="sm" variant="outline" disabled={revokeInviteM.isPending} onClick={() => revokeInviteM.mutate(invite.user_id)}>Revoke</Button>
              </div>
            </div>
          )) : <p className="text-sm text-muted-foreground">No pending invitations.</p>}
        </Panel>
      </StaggerItem>

      <StaggerItem>
        <UserPolicyPanel />
      </StaggerItem>

      <StaggerItem>
        <Panel flush>
          {selectedIds.size > 0 ? (
            // When at least one row is selected, the toolbar swaps to a
            // bulk-action bar. Tinted background makes it obvious the
            // primary actions in this view are batch operations now.
            <div className="flex flex-wrap items-center gap-2 border-b border-border bg-amber-500/5 p-2">
              <span className="font-mono text-xs text-foreground">
                {selectedIds.size} selected
              </span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => setBulkSuspendOpen(true)}
              >
                Suspend selected
              </Button>
              <Button
                size="sm"
                variant="destructive"
                onClick={() => setBulkDeleteOpen(true)}
              >
                <IconTrash className="size-3.5" />
                Delete selected
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={clearSelection}
                className="ml-auto h-8 text-xs"
              >
                <IconX className="size-3.5" />
                Clear
              </Button>
            </div>
          ) : null}
          <div className="flex flex-wrap items-center gap-2 border-b border-border p-2">
            <div className="relative w-64">
              <IconSearch className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Filter email…"
                className="h-8 pl-8"
              />
            </div>
            <Select
              value={statusFilter}
              onValueChange={(v) => setStatusFilter(v as StatusFilter)}
            >
              <SelectTrigger className="h-8 w-[160px] text-xs">
                <SelectValue placeholder="Status" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All statuses</SelectItem>
                <SelectItem value="active">Active</SelectItem>
                <SelectItem value="suspended">Suspended</SelectItem>
                <SelectItem value="pending_verification">
                  Pending verify
                </SelectItem>
                <SelectItem value="deleted">Deleted</SelectItem>
              </SelectContent>
            </Select>
            <Select
              value={roleFilter}
              onValueChange={(v) => setRoleFilter(v as RoleFilter)}
            >
              <SelectTrigger className="h-8 w-[140px] text-xs">
                <SelectValue placeholder="Role" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All roles</SelectItem>
                <SelectItem value="admin">Admin</SelectItem>
                <SelectItem value="user">User</SelectItem>
              </SelectContent>
            </Select>
            <Select
              value={totpFilter}
              onValueChange={(v) => setTotpFilter(v as TotpFilter)}
            >
              <SelectTrigger className="h-8 w-[120px] text-xs">
                <SelectValue placeholder="2FA" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">2FA: any</SelectItem>
                <SelectItem value="on">2FA: on</SelectItem>
                <SelectItem value="off">2FA: off</SelectItem>
              </SelectContent>
            </Select>
            {filtersActive && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setSearch("")
                  setStatusFilter("all")
                  setRoleFilter("all")
                  setTotpFilter("all")
                }}
                className="h-8 text-xs"
              >
                <IconX className="size-3.5" />
                Clear
              </Button>
            )}
          </div>

          {usersQ.isLoading && (
            <div className="flex flex-col gap-2 p-4">
              <Skeleton className="h-10 rounded-none" />
              <Skeleton className="h-10 rounded-none" />
              <Skeleton className="h-10 rounded-none" />
            </div>
          )}
          {usersQ.data && (
            <div className="zv-table-scroll">
              <table className="zv-table">
                <thead>
                  <tr>
                    <th className="w-8 pl-3">
                      <Checkbox
                        aria-label="Select all on this page"
                        checked={
                          allOnPageSelected
                            ? true
                            : someOnPageSelected
                              ? "indeterminate"
                              : false
                        }
                        onCheckedChange={(v) => togglePageSelection(v === true)}
                        disabled={selectableIds.length === 0}
                      />
                    </th>
                    <th>Email</th>
                    <th className="hidden md:table-cell">Role</th>
                    <th>Status</th>
                    <th className="hidden lg:table-cell">2FA</th>
                    <th className="zv-num hidden md:table-cell">Devices</th>
                    <th className="hidden lg:table-cell">Last login</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {items.map((u) => {
                    const isSelf = u.id === self?.id
                    const isChecked = selectedIds.has(u.id)
                    return (
                      <tr
                        key={u.id}
                        className={isChecked ? "bg-amber-500/5" : undefined}
                      >
                        <td className="w-8 pl-3">
                          {!isSelf && (
                            <Checkbox
                              aria-label={`Select ${u.email}`}
                              checked={isChecked}
                              onCheckedChange={(v) =>
                                toggleRowSelection(u.id, v === true)
                              }
                            />
                          )}
                        </td>
                        <td>
                          <Link
                            to={`/admin/users/${u.id}`}
                            className="inline-flex items-center gap-2 transition-colors hover:text-primary"
                          >
                            <span className="flex size-6 shrink-0 items-center justify-center border border-border bg-card p-0.5">
                              <Identicon seed={u.email} size={20} cells={5} />
                            </span>
                            <span className="font-medium underline-offset-2 hover:underline">
                              {u.email}
                            </span>
                          </Link>
                          {isSelf && (
                            <span className="ml-2 font-mono text-[10px] text-muted-foreground/60 uppercase">
                              you
                            </span>
                          )}
                        </td>
                        <td className="hidden md:table-cell">
                          {u.role === "admin" ? (
                            <Pill tone="info" dot={false}>
                              admin
                            </Pill>
                          ) : (
                            <span className="text-muted-foreground">user</span>
                          )}
                        </td>
                        <td>
                          <StatusPill
                            status={USER_STATUS_TO_PILL[u.status] ?? "pending"}
                            label={u.status.replace(/_/g, " ")}
                          />
                        </td>
                        <td className="hidden lg:table-cell">
                          {u.totp_enabled ? (
                            <Pill tone="ok" dot={false}>
                              on
                            </Pill>
                          ) : (
                            <Pill tone="warn" dot={false}>
                              off
                            </Pill>
                          )}
                        </td>
                        <td className="zv-num hidden md:table-cell">
                          {u.device_count}
                        </td>
                        <td className="hidden font-mono text-xs text-muted-foreground lg:table-cell">
                          <RelativeTime
                            value={u.last_login_at}
                            fallback="Never"
                          />
                        </td>
                        <td className="zv-actions">
                          <RowActions
                            user={u}
                            isSelf={isSelf}
                            onImpersonate={() => setImpersonateTarget(u)}
                            onSuspend={() => setSuspendTarget(u)}
                            onUnsuspend={() =>
                              setStatusM.mutate({ id: u.id, status: "active" })
                            }
                            onForceLogout={() => revokeSessionsM.mutate(u.id)}
                            onDisable2fa={() =>
                              setConfirmAction({ user: u, kind: "disable2fa" })
                            }
                            onToggleRole={() =>
                              setConfirmAction({
                                user: u,
                                kind: u.role === "admin" ? "demote" : "promote",
                              })
                            }
                            onDelete={() =>
                              setConfirmAction({ user: u, kind: "delete" })
                            }
                          />
                        </td>
                      </tr>
                    )
                  })}
                  {!usersQ.isLoading && items.length === 0 && (
                    <tr>
                      <td
                        colSpan={8}
                        className="py-8 text-center font-mono text-sm text-muted-foreground"
                      >
                        No users match.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          )}
          <Pagination
            page={page}
            pageSize={pageSize}
            total={usersQ.data?.total ?? 0}
            itemCount={items.length}
            fetching={usersQ.isFetching}
            onPageChange={setPage}
            onPageSizeChange={setPageSize}
          />
        </Panel>
      </StaggerItem>

      <ConfirmDialog
        open={!!suspendTarget}
        onOpenChange={(o) => !o && setSuspendTarget(null)}
        title={`Suspend ${suspendTarget?.email ?? "user"}?`}
        description="Suspends the account, blocks future logins, and keeps device rows in place. Reversible — Unsuspend brings them back."
        confirmLabel="Suspend"
        destructive
        pending={setStatusM.isPending}
        onConfirm={() =>
          suspendTarget &&
          setStatusM.mutate({ id: suspendTarget.id, status: "suspended" })
        }
      />

      <ConfirmDialog
        open={!!impersonateTarget}
        onOpenChange={(o) => !o && setImpersonateTarget(null)}
        title={`Impersonate ${impersonateTarget?.email ?? "user"}?`}
        description="You will be redirected to the dashboard acting as this user. A banner will remind you to exit impersonation when done."
        confirmLabel="Impersonate"
        pending={impersonateM.isPending}
        onConfirm={() =>
          impersonateTarget && impersonateM.mutate(impersonateTarget.id)
        }
      />

      <ConfirmDialog
        open={bulkSuspendOpen}
        onOpenChange={setBulkSuspendOpen}
        title={`Suspend ${selectedIds.size} selected user${selectedIds.size === 1 ? "" : "s"}?`}
        description="Each will have their dashboard access revoked, sessions invalidated, and live VPN tunnels torn down. Reversible — Unsuspend per row."
        confirmLabel={`Suspend ${selectedIds.size}`}
        destructive
        pending={bulkSuspendM.isPending}
        onConfirm={() => bulkSuspendM.mutate(Array.from(selectedIds))}
      />

      <ConfirmDialog
        open={bulkDeleteOpen}
        onOpenChange={setBulkDeleteOpen}
        title={`Delete ${selectedIds.size} selected user${selectedIds.size === 1 ? "" : "s"}?`}
        description="Permanently deletes each account and ALL their data — peers removed and every device, session, log, and preference purged from the database. This is irreversible."
        confirmLabel={`Delete ${selectedIds.size}`}
        destructive
        pending={bulkDeleteM.isPending}
        onConfirm={() => bulkDeleteM.mutate(Array.from(selectedIds))}
      />

      {confirmAction &&
        (() => {
          const { user: cu, kind } = confirmAction
          const meta = {
            delete: {
              title: `Delete ${cu.email}?`,
              description:
                "Permanently deletes this account and ALL their data — peers removed and every device, session, log, and preference purged. Irreversible.",
              confirmLabel: "Delete",
              destructive: true,
              pending: deleteM.isPending,
              onConfirm: () => deleteM.mutate(cu.id),
            },
            promote: {
              title: `Promote ${cu.email} to admin?`,
              description:
                "Grants full administrative access to the entire fleet, every user, and all settings.",
              confirmLabel: "Promote",
              destructive: false,
              pending: setRoleM.isPending,
              onConfirm: () => setRoleM.mutate({ id: cu.id, role: "admin" }),
            },
            demote: {
              title: `Demote ${cu.email} to user?`,
              description:
                "Revokes administrative access. The account and its devices are kept.",
              confirmLabel: "Demote",
              destructive: true,
              pending: setRoleM.isPending,
              onConfirm: () => setRoleM.mutate({ id: cu.id, role: "user" }),
            },
            disable2fa: {
              title: `Disable 2FA for ${cu.email}?`,
              description:
                "Clears their TOTP secret and recovery codes — they can re-enroll on next sign-in. Use only for account recovery.",
              confirmLabel: "Disable 2FA",
              destructive: true,
              pending: disable2faM.isPending,
              onConfirm: () => disable2faM.mutate(cu.id),
            },
          }[kind]
          return (
            <ConfirmDialog
              open
              onOpenChange={(o) => !o && setConfirmAction(null)}
              title={meta.title}
              description={meta.description}
              confirmLabel={meta.confirmLabel}
              destructive={meta.destructive}
              pending={meta.pending}
              onConfirm={meta.onConfirm}
            />
          )
        })()}

      <InviteUserDialog
        open={inviteOpen}
        onOpenChange={setInviteOpen}
        onCreated={() => {
          void qc.invalidateQueries({ queryKey: ["admin", "users"] })
          void qc.invalidateQueries({ queryKey: ["admin", "invitations"] })
        }}
      />
    </PageStagger>
  )
}

// ──────────────────────────────────────────────────────────────────────

/** Global non-admin policy toggles. Edits hit `/admin/user-policy`, which
 *  is reflected in every session's next `/me` (login responses also carry
 *  it for first-paint route gating). Admins are exempt from every toggle. */
function UserPolicyPanel() {
  const qc = useQueryClient()
  const setSelfUser = useAuth((s) => s.setUser)
  const selfUser = useAuth((s) => s.user)
  const q = useQuery({
    queryKey: ["admin", "user-policy"],
    queryFn: adminGetUserPolicy,
  })
  const m = useMutation({
    mutationFn: (next: UserPolicySnapshot) => adminSetUserPolicy(next),
    onSuccess: async (_d, next) => {
      // Refresh the cached row + the live auth-store snapshot so the
      // editor sees the new value immediately (and so any admin-side
      // gating that reads from auth picks it up without a reload).
      void qc.invalidateQueries({ queryKey: ["admin", "user-policy"] })
      if (selfUser) {
        setSelfUser({ ...selfUser, user_policy: next })
      }
      toast.success("User policy updated")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const policy = q.data
  return (
    <Panel
      title="User policy"
      sub="Global gates for non-admin accounts · admins are exempt"
    >
      <div className="flex items-center justify-between gap-4 py-1">
        <div className="flex flex-col">
          <span className="font-mono text-sm font-medium text-foreground">
            Hide device detail page
          </span>
          <span className="font-mono text-[11px] text-muted-foreground">
            Users still see their device list but can't open /app/devices/
            {"{id}"} — useful when the per-device charts and activity log should
            stay admin-only.
          </span>
        </div>
        <Switch
          aria-label="Hide device detail page"
          checked={!!policy?.hide_device_detail}
          disabled={q.isLoading || m.isPending}
          onCheckedChange={(checked) =>
            policy &&
            m.mutate({ ...policy, hide_device_detail: checked === true })
          }
        />
      </div>
    </Panel>
  )
}

// ──────────────────────────────────────────────────────────────────────

/** Per-row 3-dot menu surfacing the full admin action set inline, so common
 *  operations don't require opening the detail page. Mirrors the actions on
 *  the user-detail page; the parent owns the confirm dialogs and mutations. */
function RowActions({
  user,
  isSelf,
  onImpersonate,
  onSuspend,
  onUnsuspend,
  onForceLogout,
  onDisable2fa,
  onToggleRole,
  onDelete,
}: {
  user: AdminUser
  isSelf: boolean
  onImpersonate: () => void
  onSuspend: () => void
  onUnsuspend: () => void
  onForceLogout: () => void
  onDisable2fa: () => void
  onToggleRole: () => void
  onDelete: () => void
}) {
  const navigate = useNavigate()
  const active = user.status === "active"
  const suspended = user.status === "suspended"
  // Deleted accounts have nothing actionable beyond viewing the record.
  const terminal = user.status === "deleted"
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          size="icon"
          variant="ghost"
          className="size-8"
          aria-label={`Actions for ${user.email}`}
        >
          <IconDotsVertical className="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-52">
        <DropdownMenuLabel className="truncate">{user.email}</DropdownMenuLabel>
        <DropdownMenuItem onClick={() => navigate(`/admin/users/${user.id}`)}>
          <IconEye className="size-4" />
          View details
        </DropdownMenuItem>

        {!isSelf && !terminal && (
          <>
            <DropdownMenuSeparator />
            {active && (
              <DropdownMenuItem onClick={onImpersonate}>
                <IconLogin2 className="size-4" />
                Impersonate
              </DropdownMenuItem>
            )}
            {active && (
              <DropdownMenuItem onClick={onSuspend}>
                <IconUserX className="size-4" />
                Suspend
              </DropdownMenuItem>
            )}
            {suspended && (
              <DropdownMenuItem onClick={onUnsuspend}>
                <IconUserCheck className="size-4" />
                Unsuspend
              </DropdownMenuItem>
            )}

            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={onForceLogout}>
              <IconLogout className="size-4" />
              Force log out
            </DropdownMenuItem>
            {user.totp_enabled && (
              <DropdownMenuItem onClick={onDisable2fa}>
                <IconShieldOff className="size-4" />
                Disable 2FA
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onClick={onToggleRole}>
              <IconUserShield className="size-4" />
              {user.role === "admin" ? "Demote to user" : "Promote to admin"}
            </DropdownMenuItem>

            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={onDelete}>
              <IconTrash className="size-4" />
              Delete user
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

// ──────────────────────────────────────────────────────────────────────

function InviteUserDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean
  onOpenChange: (o: boolean) => void
  onCreated: () => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {/* Keyed on `open` so each open mounts a clean slate. */}
      <InviteUserDialogBody
        key={String(open)}
        onOpenChange={onOpenChange}
        onCreated={onCreated}
      />
    </Dialog>
  )
}

function InviteUserDialogBody({
  onOpenChange,
  onCreated,
}: {
  onOpenChange: (o: boolean) => void
  onCreated: () => void
}) {
  const [email, setEmail] = useState("")
  const [role, setRole] = useState<UserRole>("user")

  const m = useMutation({
    mutationFn: (body: AdminCreateUserBody) => adminCreateUser(body),
    onSuccess: () => {
      onCreated()
      toast.success("Invitation created")
      onOpenChange(false)
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
    },
  })

  const submit = () => {
    const trimmed = email.trim().toLowerCase()
    if (!trimmed.includes("@")) {
      toast.error("Enter a valid email")
      return
    }
    m.mutate({
      email: trimmed,
      role,
    })
  }

  return (
    <DialogContent className="sm:max-w-md">
      <>
          <DialogHeader>
            <DialogTitle>Invite user</DialogTitle>
            <DialogDescription>
              The user receives a one-time verification link, then signs in
              only with the matching Google account. No password is created.
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3">
            <div className="flex flex-col gap-1">
              <Label htmlFor="invite-email">Email</Label>
              <Input
                id="invite-email"
                type="email"
                inputMode="email"
                autoComplete="off"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="user@example.com"
              />
            </div>
            <div className="flex flex-col gap-1">
              <Label>Role</Label>
              <Select
                value={role}
                onValueChange={(v) => setRole(v as UserRole)}
              >
                <SelectTrigger className="text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="user">User</SelectItem>
                  <SelectItem value="admin">Admin</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button disabled={m.isPending} onClick={submit}>
              {m.isPending ? "Sending…" : "Send invitation"}
            </Button>
          </DialogFooter>
      </>
    </DialogContent>
  )
}
