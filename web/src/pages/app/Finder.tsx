import { useQueries } from "@tanstack/react-query"
import {
  IconChevronRight,
  IconClipboardList,
  IconLogin2,
  IconNetwork,
  IconRoute,
  IconSearch,
  IconUserSearch,
  IconX,
} from "@tabler/icons-react"
import { useMemo, useState } from "react"
import { Link } from "react-router"

import {
  FinderDeviceCard,
  OwnerAccordion,
} from "@/components/finder/FinderResults"
import { PageStagger, StaggerItem } from "@/components/motion"
import { parseTokens } from "@/lib/finder"
import { Kbd, PageHead, Panel, Pill } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import {
  type FinderResponse,
  type FinderUserMatch,
  type FinderDeviceMatch,
  finder,
} from "@/lib/api"
import { useAuth } from "@/stores/auth"

/**
 * Shared Finder — paste an IP, a WG `host:port` endpoint, an email, a
 * device name, or a User-Agent fragment. The backend detects what you
 * gave it, runs targeted COUNT queries across every relevant log
 * table, and returns a small set of direct matches (users / devices)
 * plus per-table counts. Admins may deep-link into management pages.
 *
 * Phase 2 / Stage B — last item in the per-user logging stack.
 */
export function FinderPage() {
  const isAdmin = useAuth((s) => s.user?.role === "admin")
  const [input, setInput] = useState("")
  const [committed, setCommitted] = useState("")
  // Regex mode: when on, we wrap the input as `/.../` before submit so
  // the backend's detect_kind picks up "regex" and runs `~*` queries
  // against IP / action / user-agent / email / endpoint columns. Mirrors
  // the VS Code search affordance — a `.*` toggle next to the input.
  const [regex, setRegex] = useState(false)

  // One committed search can hold several lookup values — paste a list
  // of IPs separated by spaces / commas / newlines and each token runs
  // as its own backend query, merged below into one owner-grouped view.
  // Regex mode (or a slash-wrapped pattern) always stays a single query
  // since the pattern itself may contain separators.
  const tokens = useMemo(() => {
    if (!committed) return []
    const isRegexQuery =
      committed.length >= 3 &&
      committed.startsWith("/") &&
      committed.endsWith("/")
    return isRegexQuery ? [committed] : parseTokens(committed)
  }, [committed])

  // Empty committed = no fetch. Page renders a help card instead.
  const queries = useQueries({
    queries: tokens.map((t) => ({
      queryKey: ["finder", t] as const,
      queryFn: () => finder(t),
      placeholderData: (prev: FinderResponse | undefined) => prev,
    })),
  })
  const anyLoading = queries.some((qq) => qq.isLoading && !qq.data)
  const firstError = queries.find((qq) => qq.isError)?.error
  // Index-aligned with `tokens`; undefined while that token's query is
  // in flight.
  const results = queries.map((qq) => qq.data)

  // Wrap raw input for the backend. Regex mode auto-slashes when the
  // user hasn't already done it themselves, so they can type a literal
  // pattern like `10\.10\.0\..*` (or even the looser `10.10.0.*`) and
  // hit Enter. If the value is already slash-wrapped we leave it.
  const wrap = (v: string, useRegex: boolean = regex): string => {
    if (!useRegex) return v
    if (v.length >= 3 && v.startsWith("/") && v.endsWith("/")) return v
    return `/${v}/`
  }

  // Submit-on-Enter or button click; explicit so a slow query / typo
  // doesn't fire on every keystroke.
  const submit = () => {
    const v = input.trim()
    if (v.length === 0) {
      setCommitted("")
      return
    }
    setCommitted(wrap(v))
  }

  // Clear the input + the committed query together so the help card
  // comes back.
  const clear = () => {
    setInput("")
    setCommitted("")
  }

  // Flip regex mode and re-run any live query in the new mode — otherwise
  // toggling looks broken (the button changes but results don't update
  // until you press Enter again).
  const toggleRegex = () => {
    const next = !regex
    setRegex(next)
    const v = input.trim()
    if (v.length > 0) setCommitted(wrap(v, next))
  }

  return (
    <PageStagger>
      <StaggerItem>
        <PageHead
          eyebrow="Workspace · Finder"
          title="Cross-source search"
          sub="Search VPN IPs, endpoints, owners, devices, and activity. Admins can open matching management pages."
        />
      </StaggerItem>

      <StaggerItem>
        <Panel flush>
          <div className="flex items-center gap-2 border-b border-border p-2">
            <div className="relative flex-1">
              <IconSearch className="absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit()
                }}
                onPaste={(e) => {
                  // Multi-line clipboard lists (one IP per line) lose
                  // their newlines in a single-line input — normalize
                  // to spaces so each value survives as a token. Regex
                  // patterns are left untouched.
                  if (regex) return
                  const text = e.clipboardData.getData("text")
                  if (/[\r\n]/.test(text)) {
                    e.preventDefault()
                    const cleaned = text.replace(/[\s,]+/g, " ").trim()
                    setInput((cur) =>
                      cur.trim() ? `${cur.trim()} ${cleaned}` : cleaned
                    )
                  }
                }}
                placeholder={
                  regex
                    ? "10\\.10\\.0\\..*  ·  curl|wget|python  ·  ^api\\/v1\\/(login|register)$"
                    : "203.0.113.42 · 203.0.113.42:51820 · curl/8 · alice@example.com · laptop-pro"
                }
                className="h-9 pl-9 font-mono text-sm"
                autoFocus
              />
              {regex && (
                <Pill
                  tone="info"
                  dot={false}
                  className="pointer-events-none absolute top-1/2 right-2 -translate-y-1/2"
                >
                  regex
                </Pill>
              )}
            </div>
            <button
              type="button"
              onClick={toggleRegex}
              aria-pressed={regex}
              aria-label="Toggle regex mode"
              title={
                regex
                  ? "Regex mode — input is matched as a POSIX regex"
                  : "Plain mode — click to enable regex (matches against IP, user-agent, endpoint, etc.)"
              }
              className={
                "inline-flex h-9 w-9 shrink-0 items-center justify-center border border-border font-mono text-[12px] transition-colors focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none " +
                (regex
                  ? "border-primary bg-primary/10 text-primary"
                  : "text-muted-foreground hover:border-foreground hover:text-foreground")
              }
            >
              .*
            </button>
            {input && (
              <Button
                size="lg"
                variant="ghost"
                onClick={clear}
                title="Clear"
                className="h-9 w-9 px-0"
              >
                <IconX className="size-3.5" />
              </Button>
            )}
            <Button
              size="lg"
              onClick={submit}
              disabled={input.trim().length === 0}
              className="h-9"
            >
              Search
            </Button>
          </div>

          {committed === "" && <FinderHelp />}
          {committed !== "" && anyLoading && (
            <div className="flex flex-col gap-2 p-4">
              <Skeleton className="h-8 rounded-none" />
              <Skeleton className="h-8 rounded-none" />
              <Skeleton className="h-8 rounded-none" />
            </div>
          )}
          {committed !== "" && !anyLoading && firstError != null && (
            <div className="p-6 font-mono text-sm text-destructive">
              Search failed:{" "}
              {firstError instanceof Error
                ? firstError.message
                : "unknown error"}
            </div>
          )}
          {committed !== "" &&
            !anyLoading &&
            firstError == null &&
            tokens.length === 1 &&
            results[0] && (
              <FinderResults data={results[0]} isAdmin={isAdmin} />
            )}
          {committed !== "" &&
            !anyLoading &&
            firstError == null &&
            tokens.length > 1 && (
              <MultiFinderResults
                tokens={tokens}
                results={results}
                isAdmin={isAdmin}
              />
            )}
        </Panel>
      </StaggerItem>
    </PageStagger>
  )
}

function FinderHelp() {
  return (
    <div className="space-y-2 p-6 font-mono text-[12px] leading-relaxed text-muted-foreground">
      <p className="text-foreground">Examples:</p>
      <ul className="space-y-1.5">
        <li>
          <Kbd>203.0.113.42</Kbd> — every audit / failed-login / session /
          access-log row that touched that IP, plus any device that connected
          from it.
        </li>
        <li>
          <Kbd>203.0.113.42:51820</Kbd> — exact WG `host:port` lookup against
          the peer endpoint history.
        </li>
        <li>
          <Kbd>10.10.0.5, 10.10.0.7 10.10.0.9</Kbd> — several values at once
          (comma / space / newline separated). Each runs its own lookup; results
          merge into one owner-grouped view.
        </li>
        <li>
          <Kbd>alice@example.com</Kbd> — substring email match. Returns up to 10
          users.
        </li>
        <li>
          <Kbd>laptop-pro</Kbd> — substring device name match.
        </li>
        <li>
          <Kbd>curl/8</Kbd> · <Kbd>python-requests</Kbd> — User-Agent substring
          across audit / failed-login / session / access-log tables.
        </li>
        <li>
          <Kbd>.*</Kbd> button (right of the input) — flip to regex mode. POSIX
          regex, case-insensitive, matched against user-agent / action / path /
          email / device name / IP / WG endpoint. With regex on, just type the
          pattern — no slashes needed: e.g. <Kbd>10\.10\.0\..*</Kbd> (every IP
          in 10.10.0.0/24), <Kbd>curl|wget|python</Kbd>,{" "}
          <Kbd>^api/v1/(login|register)$</Kbd>. Max 200 chars; invalid regex
          returns a 422. You can also type the slashed form directly without
          toggling: <Kbd>{"/pattern/"}</Kbd>.
        </li>
      </ul>
    </div>
  )
}

function FinderResults({
  data,
  isAdmin,
}: {
  data: FinderResponse
  isAdmin: boolean
}) {
  const { kind, query, counts, users, devices } = data
  const anyCount =
    counts.audit_logs +
      counts.failed_logins +
      counts.session_events +
      counts.access_logs +
      counts.peer_endpoint_history +
      counts.connection_sessions >
    0
  const anyMatch = users.length + devices.length > 0
  return (
    <div className="flex flex-col gap-4 p-4">
      <div className="flex items-center gap-2 font-mono text-[11px]">
        <span className="text-muted-foreground">Detected as</span>
        <Pill tone="info" dot={false}>
          {kind}
        </Pill>
        <span className="text-muted-foreground">for</span>
        <span className="text-foreground">{query}</span>
      </div>

      {!anyCount && !anyMatch && (
        <div className="py-8 text-center font-mono text-sm text-muted-foreground">
          Nothing matched. Try a partial value (email substring, UA fragment) or
          a different shape (full <code>host:port</code> vs. bare IP).
        </div>
      )}

      {anyCount && (
        <div>
          <h3 className="zv-eyebrow mb-2">Log counts</h3>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <CountCard
              icon={IconClipboardList}
              label="Audit"
              count={counts.audit_logs}
              link={isAdmin ? linkForAudit(kind, query) : null}
            />
            <CountCard
              icon={IconLogin2}
              label="Sessions"
              count={counts.session_events}
              link={isAdmin ? linkForSessionEvents(kind, query) : null}
            />
            <CountCard
              icon={IconRoute}
              label="Access logs"
              count={counts.access_logs}
              link={isAdmin ? linkForAccessLogs(kind, query) : null}
            />
            <CountCard
              icon={IconUserSearch}
              label="Failed logins"
              count={counts.failed_logins}
              link={isAdmin ? "/admin/failed-logins" : null}
            />
            <CountCard
              icon={IconNetwork}
              label="Peer endpoints"
              count={counts.peer_endpoint_history}
              link={null}
            />
            <CountCard
              icon={IconNetwork}
              label="Connections"
              count={counts.connection_sessions}
              link={null}
            />
          </div>
        </div>
      )}

      {users.length > 0 && (
        <div>
          <h3 className="zv-eyebrow mb-2">User matches</h3>
          <div className="flex flex-col divide-y divide-border border border-border">
            {users.map((u) => (
              <UserMatchRow key={u.id} u={u} isAdmin={isAdmin} />
            ))}
          </div>
        </div>
      )}

      {devices.length > 0 && (
        <div>
          <h3 className="zv-eyebrow mb-2">Device matches · by owner</h3>
          <DeviceOwnerGroups devices={devices} isAdmin={isAdmin} />
        </div>
      )}
    </div>
  )
}

/** Group device matches by their owning user and render one accordion
 *  section per owner, each holding live device cards (IP · rate · chart).
 *  This is the "who does this IP belong to" answer: the owner's email is
 *  the section header, the device holding the address sits inside. */
function DeviceOwnerGroups({
  devices,
  isAdmin,
}: {
  devices: FinderDeviceMatch[]
  isAdmin: boolean
}) {
  const groups = new Map<
    string,
    { email: string; devices: FinderDeviceMatch[] }
  >()
  for (const d of devices) {
    const g = groups.get(d.user_id)
    if (g) g.devices.push(d)
    else groups.set(d.user_id, { email: d.user_email, devices: [d] })
  }
  return (
    <div className="flex flex-col gap-2">
      {[...groups.entries()].map(([userId, g]) => (
        <OwnerAccordion
          key={userId}
          email={g.email}
          count={g.devices.length}
          to={isAdmin ? `/admin/users/${userId}` : undefined}
        >
          {g.devices.map((d) => (
            <FinderDeviceCard
              key={d.id}
              deviceId={d.id}
              name={d.name}
              ip={d.allocated_ip}
              to={isAdmin ? `/admin/devices/${d.id}` : undefined}
              online={isAdmin ? undefined : null}
              note={
                d.matched_on === "last_peer_endpoint"
                  ? "matched source endpoint"
                  : undefined
              }
            />
          ))}
        </OwnerAccordion>
      ))}
    </div>
  )
}

/** Merged view for a multi-value search (several IPs pasted at once).
 *  Shows a per-value ownership summary, then every matched device
 *  grouped under its owner. Log-count cards stay single-value only —
 *  their deep links can carry exactly one filter. */
function MultiFinderResults({
  tokens,
  results,
  isAdmin,
}: {
  tokens: string[]
  results: (FinderResponse | undefined)[]
  isAdmin: boolean
}) {
  const devices: FinderDeviceMatch[] = []
  const seenDev = new Set<string>()
  const users: FinderUserMatch[] = []
  const seenUser = new Set<string>()
  for (const r of results) {
    if (!r) continue
    for (const d of r.devices) {
      if (!seenDev.has(d.id)) {
        seenDev.add(d.id)
        devices.push(d)
      }
    }
    for (const u of r.users) {
      if (!seenUser.has(u.id)) {
        seenUser.add(u.id)
        users.push(u)
      }
    }
  }
  const anyMatch = devices.length + users.length > 0

  return (
    <div className="flex flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center gap-2 font-mono text-[11px]">
        <span className="text-muted-foreground">Looked up</span>
        <Pill tone="info" dot={false}>
          {tokens.length} values
        </Pill>
        <span className="text-muted-foreground">
          · log counts are shown for single-value searches
        </span>
      </div>

      <div>
        <h3 className="zv-eyebrow mb-2">Ownership</h3>
        <div className="flex flex-col divide-y divide-border border border-border">
          {tokens.map((t, i) => (
            <TokenOwnershipRow
              key={t}
              token={t}
              result={results[i]}
              isAdmin={isAdmin}
            />
          ))}
        </div>
      </div>

      {devices.length > 0 && (
        <div>
          <h3 className="zv-eyebrow mb-2">Device matches · by owner</h3>
          <DeviceOwnerGroups devices={devices} isAdmin={isAdmin} />
        </div>
      )}

      {users.length > 0 && (
        <div>
          <h3 className="zv-eyebrow mb-2">User matches</h3>
          <div className="flex flex-col divide-y divide-border border border-border">
            {users.map((u) => (
              <UserMatchRow key={u.id} u={u} isAdmin={isAdmin} />
            ))}
          </div>
        </div>
      )}

      {!anyMatch && (
        <div className="py-8 text-center font-mono text-sm text-muted-foreground">
          None of the values matched a user or device.
        </div>
      )}
    </div>
  )
}

/** One line of the multi-value summary: the searched value → who holds
 *  it. An exact `allocated_ip` hit names the owner; an endpoint-only hit
 *  means the address was a connection *source*, not a VPN peer address. */
function TokenOwnershipRow({
  token,
  result,
  isAdmin,
}: {
  token: string
  result: FinderResponse | undefined
  isAdmin: boolean
}) {
  const holder = result?.devices.find((d) => d.allocated_ip === token)
  const sourceOnly =
    !holder &&
    (result?.devices.some((d) => d.matched_on === "last_peer_endpoint") ??
      false)
  return (
    <div className="flex items-center justify-between gap-3 px-3 py-2 font-mono text-xs">
      <span className="shrink-0 text-foreground">{token}</span>
      {holder && isAdmin ? (
        <Link
          to={`/admin/users/${holder.user_id}`}
          className="min-w-0 truncate text-right text-muted-foreground hover:text-foreground"
        >
          <span className="text-foreground">{holder.user_email}</span>
          <span className="px-1 opacity-60">·</span>
          {holder.name}
        </Link>
      ) : holder ? (
        <span className="min-w-0 truncate text-right text-muted-foreground">
          <span className="text-foreground">{holder.user_email}</span>
          <span className="px-1 opacity-60">·</span>
          {holder.name}
        </span>
      ) : sourceOnly ? (
        <span className="text-muted-foreground">
          connection source only — no peer holds it
        </span>
      ) : (
        <span className="text-muted-foreground opacity-60">no match</span>
      )}
    </div>
  )
}

function CountCard({
  icon: Icon,
  label,
  count,
  link,
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  count: number
  link: string | null
}) {
  const inner = (
    <div className="group flex h-full flex-col gap-1 border border-border bg-card p-3 transition hover:border-foreground/40">
      <div className="flex items-center justify-between font-mono text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
        <span className="flex items-center gap-1.5">
          <Icon className="size-3.5" />
          {label}
        </span>
        {link && (
          <IconChevronRight className="size-3.5 text-muted-foreground group-hover:text-foreground" />
        )}
      </div>
      <div className="font-heading text-2xl text-foreground tabular-nums">
        {count.toLocaleString()}
      </div>
    </div>
  )
  if (link && count > 0) {
    return (
      <Link to={link} title={`Open ${label} filtered to this query`}>
        {inner}
      </Link>
    )
  }
  return inner
}

function UserMatchRow({
  u,
  isAdmin,
}: {
  u: FinderUserMatch
  isAdmin: boolean
}) {
  const content = (
    <>
      <div className="flex flex-col gap-0.5">
        <span className="font-mono text-sm">{u.email}</span>
        <span className="font-mono text-[10px] text-muted-foreground">
          {u.id.slice(0, 8)} · matched on {u.matched_on}
        </span>
      </div>
      {isAdmin && <IconChevronRight className="size-4 text-muted-foreground" />}
    </>
  )
  const className = "flex items-center justify-between gap-3 px-3 py-2"
  return isAdmin ? (
    <Link
      to={`/admin/users/${u.id}`}
      className={`${className} transition hover:bg-muted/40`}
    >
      {content}
    </Link>
  ) : (
    <div className={className}>{content}</div>
  )
}

// Deep-link builders. The target pages already accept `ip` / `path` /
// `user_agent` query params (see Audit + Sessions + AccessLogs).
function linkForAudit(kind: string, q: string): string | null {
  if (kind === "ip") return `/admin/audit?ip=${encodeURIComponent(q)}`
  // Audit page doesn't accept a UA filter yet; surface the count as a
  // hint and let the admin pivot manually. Returning null here makes
  // the CountCard a non-clickable info tile.
  return null
}

function linkForSessionEvents(kind: string, q: string): string | null {
  if (kind === "ip") return `/admin/sessions?ip=${encodeURIComponent(q)}`
  return null
}

function linkForAccessLogs(kind: string, q: string): string | null {
  if (kind === "ip") return `/admin/access-logs?ip=${encodeURIComponent(q)}`
  return null
}
