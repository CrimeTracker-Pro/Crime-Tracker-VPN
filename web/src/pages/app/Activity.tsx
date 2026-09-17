import { useQuery } from "@tanstack/react-query"
import { useState } from "react"

import { CopyableCode } from "@/components/CopyableCode"
import { PageStagger, StaggerItem } from "@/components/motion"
import { Pagination } from "@/components/Pagination"
import { RelativeTime } from "@/components/RelativeTime"
import { Kbd, PageHead, Panel } from "@/components/swiss"
import { Skeleton } from "@/components/ui/skeleton"
import { listMyActivity } from "@/lib/api"

/** Friendly labels for the actions a user is likely to see. Anything not
 *  mapped falls back to the raw dotted key rendered as a <Kbd>. */
const ACTION_LABELS: Record<string, string> = {
  "device.created": "Device added",
  "device.updated": "Device updated",
  "device.revoked": "Device revoked",
  "device.paused": "Device paused",
  "device.unpaused": "Device resumed",
  "device.reconnected": "Device reconnected",
  "device.keys_rotated": "Keys rotated",
  "device.conf_redownloaded": "Config re-downloaded",
  "device.reordered": "Devices reordered",
  "auth.login": "Signed in",
  "auth.logout": "Signed out",
  "auth.new_ip": "Sign-in from a new IP",
  "auth.password_changed": "Password changed",
  "auth.totp_enabled": "Two-factor enabled",
  "auth.totp_disabled": "Two-factor disabled",
  "admin.user_status_changed": "Account status changed",
  "admin.user_role_changed": "Account role changed",
  "admin.user_quota_set": "Quota updated",
  "admin.user_email_changed": "Email changed",
  "admin.user_2fa_disabled": "Two-factor reset by admin",
  "admin.user_sessions_revoked": "Sessions revoked by admin",
}

/** Humanize the target type for display ("device" → "Device"). */
function targetLabel(type: string | null): string {
  if (!type) return "—"
  if (type === "user") return "Account"
  return type.charAt(0).toUpperCase() + type.slice(1)
}

/**
 * Self-service activity log — the user-facing equivalent of the admin audit
 * log, scoped to the signed-in user's own activity (their actions + admin
 * actions on their account), newest first, paginated. Linked from the
 * dashboard's "Recent activity" panel.
 */
export function ActivityPage() {
  const [page, setPage] = useState(0)
  const [pageSize, setPageSize] = useState(50)

  const q = useQuery({
    queryKey: ["me", "activity", page, pageSize],
    queryFn: () => listMyActivity(pageSize, page * pageSize),
    placeholderData: (prev) => prev,
  })
  const items = q.data?.items ?? []
  const total = q.data?.total ?? 0

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Account"
          title="Activity"
          sub={`${total.toLocaleString()} events · your sign-ins, device changes, and account updates`}
        />
      </StaggerItem>

      <StaggerItem>
        <Panel flush>
          {q.isLoading && (
            <div className="flex flex-col gap-2 p-4">
              <Skeleton className="h-8 rounded-none" />
              <Skeleton className="h-8 rounded-none" />
              <Skeleton className="h-8 rounded-none" />
            </div>
          )}
          {q.data && (
            <div className="zv-table-scroll">
              <table className="zv-table">
                <thead>
                  <tr>
                    <th className="w-[160px]">When</th>
                    <th>Event</th>
                    <th className="hidden md:table-cell">Target</th>
                    <th className="hidden lg:table-cell">Details</th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((row) => {
                    const label = ACTION_LABELS[row.action]
                    const hasMeta =
                      row.metadata != null &&
                      typeof row.metadata === "object" &&
                      Object.keys(row.metadata).length > 0
                    return (
                      <tr key={row.id}>
                        <td className="text-muted-foreground font-mono text-xs">
                          <RelativeTime value={row.created_at} />
                        </td>
                        <td>
                          {label ? (
                            <span className="text-sm">{label}</span>
                          ) : (
                            <Kbd>{row.action}</Kbd>
                          )}
                        </td>
                        <td className="hidden font-mono text-xs md:table-cell">
                          {targetLabel(row.target_type)}
                          {row.target_id && (
                            <span className="text-muted-foreground ml-1">
                              · {row.target_id.slice(0, 8)}
                            </span>
                          )}
                        </td>
                        <td className="hidden max-w-[320px] lg:table-cell">
                          {hasMeta ? (
                            <CopyableCode
                              value={JSON.stringify(row.metadata)}
                              truncate
                            />
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </td>
                      </tr>
                    )
                  })}
                  {items.length === 0 && (
                    <tr>
                      <td
                        colSpan={4}
                        className="text-muted-foreground py-8 text-center font-mono text-sm"
                      >
                        No activity yet — your sign-ins and device changes will
                        appear here.
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
            total={total}
            itemCount={items.length}
            fetching={q.isFetching}
            onPageChange={setPage}
            onPageSizeChange={setPageSize}
          />
        </Panel>
      </StaggerItem>
    </PageStagger>
  )
}
