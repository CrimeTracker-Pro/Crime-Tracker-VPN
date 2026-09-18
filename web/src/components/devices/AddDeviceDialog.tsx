import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  IconChevronDown,
  IconCopy,
  IconDownload,
  IconEye,
  IconEyeOff,
  IconQrcode,
} from "@tabler/icons-react"
import { useMemo, useState } from "react"
import { toast } from "sonner"

import { CopyableCode } from "@/components/CopyableCode"
import { Eyebrow } from "@/components/swiss"
import { Button } from "@/components/ui/button"
import {
  SheetClose,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Switch } from "@/components/ui/switch"
import { OptionTiles } from "@/components/devices/OptionTiles"
import {
  ApiError,
  type CreatedDevice,
  type DeviceOs,
  type DeviceType,
  createDevice,
  listDevices,
  meServer,
  myUsage,
} from "@/lib/api"
import { formatBytes } from "@/lib/units"
import { copyText } from "@/lib/clipboard"
import { DEVICE_TYPE_OPTIONS, OS_OPTIONS } from "@/lib/deviceIcons"

type IpMode = "auto" | "custom"

export function AddDeviceDialog({
  onCreated,
}: {
  onCreated: (d: CreatedDevice) => void
}) {
  const qc = useQueryClient()
  // Server CIDR validates custom IP allocation.
  const serverInfoQ = useQuery({
    queryKey: ["me", "server"],
    queryFn: meServer,
    staleTime: 5 * 60_000,
  })
  const serverCidr = serverInfoQ.data?.cidr

  // Existing devices for the inline name-uniqueness check. Shares the
  // same query key the parent table uses, so cache is reused — no extra
  // network round-trip.
  const devicesQ = useQuery({ queryKey: ["devices"], queryFn: listDevices })

  // Account-level monthly cap — used as the default for the per-device cap
  // input so devices stay bounded by the account quota by default. Shares
  // the same query key the dashboard uses so the cache is reused.
  const usageQ = useQuery({ queryKey: ["me", "usage"], queryFn: myUsage })
  const accountCapBytes = usageQ.data?.monthly_byte_cap ?? null
  const existingNames = useMemo(
    () =>
      new Set((devicesQ.data ?? []).map((d) => d.name.trim().toLowerCase())),
    [devicesQ.data]
  )

  const [step, setStep] = useState<1 | 2>(1)
  const [name, setName] = useState("")
  // Empty until the user picks one — OS and type are now a required, deliberate
  // choice (no "Other" escape hatch). `canSubmit` gates the Create button on
  // both being set.
  const [osChoice, setOsChoice] = useState<DeviceOs | "">("")
  const [deviceType, setDeviceType] = useState<DeviceType | "">("")
  const [ipMode, setIpMode] = useState<IpMode>("auto")
  const [ipInput, setIpInput] = useState("")
  // Per-device monthly cap. A toggle decides whether the device is capped
  // at all: off = unlimited (no per-device cap), on = a required GB value.
  // Until the user touches either control (`capOverride` set), the toggle
  // stays off and the GB field mirrors the account cap (when there is one)
  // so enabling the toggle offers a sensible starting value to adjust — and
  // a `usageQ` refetch never clobbers the user's choice.
  const [capOverride, setCapOverride] = useState<{
    enabled: boolean
    gb: string
  } | null>(null)
  const [result, setResult] = useState<CreatedDevice | null>(null)

  const capEnabled = capOverride?.enabled ?? false
  const capGbInput = capOverride
    ? capOverride.gb
    : accountCapBytes && accountCapBytes > 0
      ? formatCapGb(accountCapBytes)
      : ""

  const nameTaken =
    name.trim().length > 0 && existingNames.has(name.trim().toLowerCase())

  const ipValidation = useMemo<{ ok: boolean; error: string | null }>(() => {
    if (ipMode === "auto") return { ok: true, error: null }
    const v = ipInput.trim()
    if (!v) return { ok: false, error: null }
    if (
      !/^(?:(?:25[0-5]|2[0-4]\d|1?\d{1,2})\.){3}(?:25[0-5]|2[0-4]\d|1?\d{1,2})$/.test(
        v
      )
    ) {
      return { ok: false, error: "that doesn't look like a valid IPv4 address" }
    }
    if (serverCidr) {
      const reason = ipOutsideCidrReason(v, serverCidr)
      if (reason) return { ok: false, error: reason }
    }
    return { ok: true, error: null }
  }, [ipMode, ipInput, serverCidr])
  const ipLooksValid = ipValidation.ok

  // Cap validation. Toggle off = unlimited (no per-device cap → null bytes).
  // Toggle on = a GB value is required and must be a positive number, clamped
  // to the account cap (also enforced server-side); surface the constraint
  // client-side so the user sees the violation before they hit submit.
  const capValidation = useMemo<{
    ok: boolean
    bytes: number | null
    error: string | null
  }>(() => {
    if (!capEnabled) return { ok: true, bytes: null, error: null }
    const raw = capGbInput.trim()
    if (!raw) {
      return { ok: false, bytes: null, error: "enter a monthly cap in GB" }
    }
    const n = Number(raw)
    if (!Number.isFinite(n) || n <= 0) {
      return {
        ok: false,
        bytes: null,
        error: "must be a positive number of GB",
      }
    }
    const bytes = Math.round(n * 1024 ** 3)
    if (accountCapBytes && bytes > accountCapBytes) {
      return {
        ok: false,
        bytes,
        error: `can't exceed your account cap (${formatBytes(accountCapBytes)})`,
      }
    }
    return { ok: true, bytes, error: null }
  }, [capEnabled, capGbInput, accountCapBytes])

  const addM = useMutation({
    mutationFn: async () => {
      if (!osChoice || !deviceType) {
        throw new ApiError(422, "validation", "Choose an OS and device type")
      }
      const created = await createDevice({
        name: name.trim(),
        os: osChoice,
        device_type: deviceType,
        allocated_ip:
          ipMode === "custom" && ipInput.trim() ? ipInput.trim() : undefined,
        monthly_byte_cap: capValidation.bytes,
      })
      return created
    },
    onSuccess: (data) => {
      setResult(data)
      setStep(2)
      void qc.invalidateQueries({ queryKey: ["devices"] })
      toast.success("Device added")
    },
    onError: (e: unknown) => {
      if (e instanceof ApiError) toast.error(e.message)
      else toast.error("Failed to create device")
    },
  })

  const canSubmit =
    name.trim().length > 0 &&
    name.trim().length <= 64 &&
    !nameTaken &&
    osChoice !== "" &&
    deviceType !== "" &&
    ipLooksValid &&
    capValidation.ok &&
    !addM.isPending

  const resetAll = () => {
    setStep(1)
    setName("")
    setOsChoice("")
    setDeviceType("")
    setIpMode("auto")
    setIpInput("")
    setResult(null)
    setCapOverride(null)
  }

  return (
    <SheetContent
      side="right"
      showCloseButton={false}
      className="flex flex-col gap-0 p-0 data-[side=right]:w-full data-[side=right]:sm:max-w-[820px]"
    >
      <SheetHeader className="border-b border-border">
        <SheetTitle>
          <Eyebrow num={`0${step}/02`}>Add device</Eyebrow>
        </SheetTitle>
        <SheetDescription>
          We generate a fresh keypair, allocate an IP, and hand you a WireGuard
          config. The private key never leaves the page.
        </SheetDescription>
      </SheetHeader>

      {step === 1 && (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-4">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="dev-name" className="zv-eyebrow">
                Device name
              </Label>
              <Input
                id="dev-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="macbook-pro · pixel-8 · home-server"
                className="font-mono"
                autoFocus
                maxLength={64}
                aria-invalid={nameTaken}
              />
              <p className="font-mono text-[11px] text-muted-foreground">
                1–64 chars. Used as the WireGuard interface name on the device.
                {nameTaken && (
                  <span className="ml-2 text-destructive">
                    you already have a device with this name
                  </span>
                )}
              </p>
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="zv-eyebrow">Operating system</Label>
              <OptionTiles
                ariaLabel="Operating system"
                options={OS_OPTIONS}
                value={osChoice}
                onChange={setOsChoice}
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="zv-eyebrow">Device type</Label>
              <OptionTiles
                ariaLabel="Device type"
                options={DEVICE_TYPE_OPTIONS}
                value={deviceType}
                onChange={setDeviceType}
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="zv-eyebrow">
                IP allocation
                {serverCidr && (
                  <span className="text-muted-foreground/70 normal-case">
                    {" "}
                    · subnet{" "}
                    <span className="font-mono text-foreground">
                      {serverCidr}
                    </span>
                  </span>
                )}
              </Label>
              <div className="grid grid-cols-2 gap-2">
                <IpModeOption
                  selected={ipMode === "auto"}
                  onClick={() => setIpMode("auto")}
                  title="Auto-assign"
                  sub={
                    serverCidr
                      ? `Server picks the next free address in ${serverCidr}.`
                      : "Server picks the next free address in the subnet."
                  }
                />
                <IpModeOption
                  selected={ipMode === "custom"}
                  onClick={() => setIpMode("custom")}
                  title="Choose IP"
                  sub={
                    serverCidr
                      ? `Reserve a specific address inside ${serverCidr}.`
                      : "Reserve a specific address inside the server's CIDR."
                  }
                />
              </div>
              {ipMode === "custom" && (
                <div className="mt-1 flex flex-col gap-2">
                  {serverCidr &&
                    (() => {
                      const range = ipRangeFromCidr(serverCidr)
                      if (!range) return null
                      return (
                        <div className="flex items-center justify-between gap-3 border border-border bg-muted/30 px-3 py-2 font-mono text-[11px]">
                          <div className="flex flex-col gap-0.5">
                            <span className="text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
                              Usable range
                            </span>
                            <span className="text-foreground tabular-nums">
                              {range.first}
                              <span className="text-muted-foreground"> → </span>
                              {range.last}
                            </span>
                          </div>
                          <div className="text-right text-muted-foreground/80">
                            <div className="tabular-nums">
                              {range.total.toLocaleString()}
                            </div>
                            <div className="text-[10px] tracking-[0.08em] uppercase">
                              addresses
                            </div>
                          </div>
                        </div>
                      )
                    })()}
                  <Input
                    value={ipInput}
                    onChange={(e) => setIpInput(e.target.value)}
                    placeholder={
                      serverCidr ? ipPlaceholderFor(serverCidr) : "10.42.0.42"
                    }
                    className="font-mono"
                    aria-invalid={!ipLooksValid}
                    autoFocus
                  />
                  <p className="font-mono text-[11px] text-muted-foreground">
                    IPv4 only. Must be inside{" "}
                    <span className="font-mono text-foreground">
                      {serverCidr ?? "the server's subnet"}
                    </span>{" "}
                    and not already taken. Network / broadcast / gateway are
                    reserved.
                    {ipValidation.error && (
                      <span className="ml-2 text-destructive">
                        {ipValidation.error}
                      </span>
                    )}
                  </p>
                </div>
              )}
            </div>

            {/* Per-device monthly cap. A toggle chooses whether the device is
              capped at all: off = unlimited (no per-device cap), on = a
              required GB value. Defaults OFF (unlimited) — the user opts in
              when they want a limit; the account cap is still enforced
              server-side regardless. */}
            <div className="flex flex-col gap-1.5">
              <div className="flex items-center justify-between gap-3">
                <Label htmlFor="dev-cap" className="zv-eyebrow">
                  Monthly cap
                  {accountCapBytes && accountCapBytes > 0 && (
                    <span className="text-muted-foreground/70 normal-case">
                      {" "}
                      · account cap{" "}
                      <span className="font-mono text-foreground">
                        {formatBytes(accountCapBytes)}
                      </span>
                    </span>
                  )}
                </Label>
                <Switch
                  checked={capEnabled}
                  onCheckedChange={(v) =>
                    setCapOverride({ enabled: v, gb: capGbInput })
                  }
                  aria-label="Enable a monthly data cap for this device"
                />
              </div>
              {capEnabled ? (
                <>
                  <div
                    data-invalid={!capValidation.ok ? "1" : undefined}
                    className="flex h-8 items-stretch overflow-hidden rounded-lg border border-input bg-transparent transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 data-[invalid=1]:border-destructive data-[invalid=1]:ring-3 data-[invalid=1]:ring-destructive/20"
                  >
                    <input
                      id="dev-cap"
                      type="number"
                      min={0}
                      step="0.1"
                      value={capGbInput}
                      onChange={(e) =>
                        setCapOverride({
                          enabled: capEnabled,
                          gb: e.target.value,
                        })
                      }
                      placeholder={
                        accountCapBytes && accountCapBytes > 0
                          ? formatCapGb(accountCapBytes)
                          : "e.g. 100"
                      }
                      className="min-w-0 flex-1 bg-transparent px-2.5 py-1 font-mono text-sm text-foreground outline-none placeholder:text-muted-foreground"
                    />
                    <span className="inline-flex shrink-0 items-center border-l border-input bg-muted/40 px-2.5 font-mono text-[12px] text-muted-foreground">
                      GB / month
                    </span>
                  </div>
                  <p className="font-mono text-[11px] text-muted-foreground">
                    This device auto-pauses when it reaches the cap this cycle.
                    {capValidation.error && (
                      <span className="ml-2 text-destructive">
                        {capValidation.error}
                      </span>
                    )}
                  </p>
                </>
              ) : (
                <p className="font-mono text-[11px] text-muted-foreground">
                  {accountCapBytes && accountCapBytes > 0
                    ? "Unlimited for this device — your account cap still applies."
                    : "Unlimited — this device has no monthly data cap."}
                </p>
              )}
            </div>
          </div>
          <SheetFooter className="flex-row justify-end gap-2 border-t border-border">
            <SheetClose asChild>
              <Button variant="ghost" disabled={addM.isPending}>
                Cancel
              </Button>
            </SheetClose>
            <Button onClick={() => addM.mutate()} disabled={!canSubmit}>
              {addM.isPending ? "Generating…" : "Generate config →"}
            </Button>
          </SheetFooter>
        </div>
      )}

      {step === 2 && result && (
        <Step2Result
          result={result}
          onDone={() => {
            onCreated(result)
            resetAll()
          }}
        />
      )}
    </SheetContent>
  )
}

/**
 * Step-2 "config ready" pane. Fills the available side-sheet height
 * with a parsed view of the generated WireGuard config — [Interface]
 * and [Peer] cards rendered as compact key/value rows — instead of
 * leaving dead space below the QR. The raw .conf is still one click
 * away via the "View raw .conf" disclosure at the bottom.
 */
function Step2Result({
  result,
  onDone,
}: {
  result: CreatedDevice
  onDone: () => void
}) {
  const serverInfoQ = useQuery({ queryKey: ["me", "server"], queryFn: meServer, staleTime: 5 * 60_000 })
  const parsed = useMemo(() => parseWgConf(result.config), [result.config])
  const [showRaw, setShowRaw] = useState(false)

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-4">
        <p className="text-[13px] leading-relaxed text-muted-foreground">
          The keypair was generated server-side for this peer; the private key
          is in the config below and{" "}
          <strong className="text-foreground">isn't stored</strong> after you
          dismiss this dialog.{" "}
          <span className="text-foreground">Save it now.</span>
        </p>

        <div className="grid gap-0 border border-border md:grid-cols-[auto_1fr]">
          <div className="flex aspect-square shrink-0 items-center justify-center border-border bg-card p-3 md:aspect-auto md:w-[300px] md:border-r">
            <span
              className="block size-[256px] max-w-full [&>svg]:size-full"
              dangerouslySetInnerHTML={{ __html: result.qr_svg }}
            />
          </div>
          <div className="flex min-w-0 flex-col gap-3 p-4">
            <div className="flex flex-col gap-1">
              <Eyebrow>Scan with WireGuard / mobile</Eyebrow>
              <p className="font-mono text-[11px] text-muted-foreground">
                Allocated IP{" "}
                <span className="text-foreground">
                  {result.device.allocated_ip}
                </span>
              </p>
            </div>
            <div className="mt-auto grid grid-cols-2 gap-2">
              <Button
                size="sm"
                onClick={() =>
                  downloadConfig(serverInfoQ.data?.name, result.config)
                }
              >
                <IconDownload size={14} />
                Download .conf
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  if (copyText(result.config)) toast.success("Config copied")
                  else toast.error("Failed to copy")
                }}
              >
                <IconQrcode size={14} />
                Copy config
              </Button>
            </div>
          </div>
        </div>

        {/* Interface / Peer cards — fill the formerly-empty space below
            the QR with the structured view of the .conf. Stack into a
            single column on narrow viewports. */}
        <div className="grid gap-3 md:grid-cols-2">
          <ConfigSection title="Interface" eyebrow="Local peer">
            <ConfigRow label="Address" value={parsed.interface.address} mono />
            <ConfigRow label="MTU" value={parsed.interface.mtu} mono />
            {parsed.interface.privateKey && (
              <SecretRow
                label="Private key"
                value={parsed.interface.privateKey}
              />
            )}
          </ConfigSection>

          <ConfigSection title="Peer" eyebrow="Remote server">
            <ConfigRow
              label="Public key"
              value={parsed.peer.publicKey}
              mono
              truncate
            />
            <ConfigRow label="Endpoint" value={parsed.peer.endpoint} mono />
            <ConfigRow
              label="Allowed IPs"
              value={parsed.peer.allowedIps}
              mono
            />
            <ConfigRow label="Keepalive" value={parsed.peer.keepalive} mono />
          </ConfigSection>
        </div>

        <details
          open={showRaw}
          onToggle={(e) => setShowRaw((e.target as HTMLDetailsElement).open)}
          className="border border-border"
        >
          <summary className="flex cursor-pointer items-center gap-2 px-3 py-2 font-mono text-[11px] tracking-[0.08em] text-muted-foreground uppercase transition-colors hover:text-foreground">
            <IconChevronDown
              className={`size-3.5 transition-transform ${showRaw ? "rotate-0" : "-rotate-90"}`}
            />
            View raw .conf
          </summary>
          <div className="max-h-[260px] overflow-y-auto border-t border-border">
            <CopyableCode value={result.config} multiline />
          </div>
        </details>
      </div>

      <SheetFooter className="flex-row justify-end gap-2 border-t border-border">
        <Button onClick={onDone}>Done</Button>
      </SheetFooter>
    </div>
  )
}

/** Bordered card with an eyebrow + title + body. Sized to match the rest
 *  of the dialog's visual rhythm. */
function ConfigSection({
  title,
  eyebrow,
  children,
}: {
  title: string
  eyebrow: string
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col border border-border">
      <div className="border-b border-border px-3 py-2">
        <div className="font-mono text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
          {eyebrow}
        </div>
        <div className="text-sm font-medium text-foreground">{title}</div>
      </div>
      <dl className="flex flex-col">{children}</dl>
    </div>
  )
}

/** Single key/value row inside a ConfigSection. Renders dt/dd pair with
 *  a hairline divider on top so consecutive rows separate cleanly. */
function ConfigRow({
  label,
  value,
  mono,
  truncate,
}: {
  label: string
  value: string | null | undefined
  mono?: boolean
  truncate?: boolean
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 border-border px-3 py-2 [&:not(:first-child)]:border-t">
      <dt className="shrink-0 font-mono text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
        {label}
      </dt>
      <dd
        className={[
          "min-w-0 text-right text-[12px] text-foreground",
          mono ? "font-mono" : "",
          truncate ? "truncate" : "break-all",
        ].join(" ")}
        title={truncate && value ? value : undefined}
      >
        {value || <span className="text-muted-foreground/60">—</span>}
      </dd>
    </div>
  )
}

/** Like ConfigRow but for the private key: masked by default with an
 *  eye-toggle to reveal and a one-click copy. The plaintext only lives
 *  in component state — it's never stored after the sheet closes. */
function SecretRow({ label, value }: { label: string; value: string }) {
  const [revealed, setRevealed] = useState(false)
  const copy = () => {
    if (copyText(value)) toast.success(`${label} copied`)
    else toast.error("Clipboard blocked — copy from the raw .conf instead")
  }
  return (
    <div className="flex items-center justify-between gap-3 border-border px-3 py-2 [&:not(:first-child)]:border-t">
      <dt className="shrink-0 font-mono text-[10px] tracking-[0.08em] text-muted-foreground uppercase">
        {label}
      </dt>
      <dd className="flex min-w-0 items-center gap-1.5">
        <span
          className={[
            "max-w-[180px] truncate font-mono text-[12px] text-foreground sm:max-w-[260px]",
            revealed ? "" : "tracking-[0.15em]",
          ].join(" ")}
        >
          {revealed ? value : "•".repeat(Math.min(24, value.length))}
        </span>
        <button
          type="button"
          onClick={() => setRevealed((v) => !v)}
          className="inline-flex size-6 shrink-0 items-center justify-center border border-border text-muted-foreground transition-colors hover:border-foreground hover:text-foreground"
          aria-label={revealed ? "Hide private key" : "Reveal private key"}
        >
          {revealed ? (
            <IconEyeOff className="size-3.5" />
          ) : (
            <IconEye className="size-3.5" />
          )}
        </button>
        <button
          type="button"
          onClick={copy}
          className="inline-flex size-6 shrink-0 items-center justify-center border border-border text-muted-foreground transition-colors hover:border-foreground hover:text-foreground"
          aria-label={`Copy ${label}`}
        >
          <IconCopy className="size-3.5" />
        </button>
      </dd>
    </div>
  )
}

interface ParsedConf {
  interface: {
    address?: string
    mtu?: string
    privateKey?: string
  }
  peer: {
    publicKey?: string
    endpoint?: string
    allowedIps?: string
    keepalive?: string
  }
}

/** Minimal WireGuard .conf parser. Walks the [section] / Key = Value
 *  lines the server emits — order-independent, comment-tolerant — and
 *  surfaces the four-or-so fields we want to show on each card. Anything
 *  unrecognised is ignored: the source of truth is still the raw .conf
 *  available through the disclosure below. */
function parseWgConf(src: string): ParsedConf {
  const out: ParsedConf = { interface: {}, peer: {} }
  let section: "interface" | "peer" | "" = ""
  for (const rawLine of src.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line || line.startsWith("#")) continue
    const sec = line.match(/^\[(.+)\]$/)
    if (sec) {
      const tag = sec[1].toLowerCase()
      section = tag === "interface" ? "interface" : tag === "peer" ? "peer" : ""
      continue
    }
    const eq = line.indexOf("=")
    if (eq < 0) continue
    const key = line.slice(0, eq).trim().toLowerCase()
    const value = line.slice(eq + 1).trim()
    if (section === "interface") {
      if (key === "address") out.interface.address = value
      else if (key === "mtu") out.interface.mtu = value
      else if (key === "privatekey") out.interface.privateKey = value
    } else if (section === "peer") {
      if (key === "publickey") out.peer.publicKey = value
      else if (key === "endpoint") out.peer.endpoint = value
      else if (key === "allowedips") out.peer.allowedIps = value
      else if (key === "persistentkeepalive") {
        out.peer.keepalive = /^\d+$/.test(value) ? `${value}s` : value
      }
    }
  }
  return out
}

function IpModeOption({
  selected,
  onClick,
  title,
  sub,
}: {
  selected: boolean
  onClick: () => void
  title: string
  sub: string
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={selected}
      className={[
        "flex flex-col items-start gap-1 border border-border p-3 text-left transition",
        selected ? "border-primary bg-primary text-primary-foreground" : "hover:border-foreground/40",
      ].join(" ")}
    >
      <span className="flex items-center gap-2 text-sm font-medium">
        <span
          className={[
            "flex size-3.5 shrink-0 items-center justify-center rounded-full border",
            selected ? "border-primary" : "border-border",
          ].join(" ")}
          aria-hidden
        >
          {selected && (
            <span className="block size-1.5 rounded-full bg-primary" />
          )}
        </span>
        {title}
      </span>
      <span className="font-mono text-[11px] leading-snug text-muted-foreground">
        {sub}
      </span>
    </button>
  )
}

/** Render a byte count as a GB string suited to a numeric `<input>` —
 *  whole GB when the value rounds cleanly, otherwise one decimal place
 *  so the user sees the exact account cap (not a misleading rounded
 *  number). Distinct from `formatBytes` which prefers human prose
 *  ("17.4 GB") and isn't reversible. */
function formatCapGb(bytes: number): string {
  const gb = bytes / 1024 ** 3
  if (gb >= 10) return Math.round(gb).toString()
  return gb.toFixed(1).replace(/\.0$/, "")
}

function downloadConfig(name: string | undefined, config: string) {
  const safe = (name ?? "crimetracker-vpn").toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 15) || "wireguard"
  const blob = new Blob([config], { type: "text/plain" })
  const url = URL.createObjectURL(blob)
  const a = document.createElement("a")
  a.href = url
  a.download = `${safe}.conf`
  a.click()
  URL.revokeObjectURL(url)
}

function ipPlaceholderFor(cidr: string): string {
  const range = ipRangeFromCidr(cidr)
  if (!range) return "10.42.0.42"
  const slash = cidr.indexOf("/")
  const prefix = slash > 0 ? parseInt(cidr.slice(slash + 1), 10) : 32
  if (prefix <= 24) {
    const parts = range.first.split(".")
    return `${parts[0]}.${parts[1]}.${parts[2]}.42`
  }
  return range.first
}

function ipOutsideCidrReason(ip: string, cidr: string): string | null {
  const slash = cidr.indexOf("/")
  if (slash < 0) return null
  const net = cidr.slice(0, slash)
  const prefix = parseInt(cidr.slice(slash + 1), 10)
  if (isNaN(prefix) || prefix < 0 || prefix > 32) return null
  const netParts = net.split(".").map(Number)
  const ipParts = ip.split(".").map(Number)
  if (netParts.length !== 4 || ipParts.length !== 4) return null
  const netU32 =
    ((netParts[0] << 24) |
      (netParts[1] << 16) |
      (netParts[2] << 8) |
      netParts[3]) >>>
    0
  const ipU32 =
    ((ipParts[0] << 24) |
      (ipParts[1] << 16) |
      (ipParts[2] << 8) |
      ipParts[3]) >>>
    0
  const total = 2 ** (32 - prefix)
  const mask = prefix === 0 ? 0 : ~((1 << (32 - prefix)) - 1) >>> 0
  const baseU32 = (netU32 & mask) >>> 0
  const broadcastU32 = (baseU32 + total - 1) >>> 0
  if (ipU32 < baseU32 || ipU32 > broadcastU32) {
    return `outside ${cidr}`
  }
  if (total >= 4) {
    if (ipU32 === baseU32) return "network address (reserved)"
    if (ipU32 === broadcastU32) return "broadcast address (reserved)"
    if (ipU32 === (baseU32 + 1) >>> 0) return "gateway address (reserved)"
  }
  return null
}

/** Compute the usable IPv4 address range for a CIDR — excluding the
 *  network address, the gateway slot (.1, which the server reserves),
 *  and the broadcast address. Returns null for malformed input or subnets
 *  too small to host anything (≤ /30). */
function ipRangeFromCidr(cidr: string): {
  first: string
  last: string
  total: number
} | null {
  const slash = cidr.indexOf("/")
  if (slash < 0) return null
  const net = cidr.slice(0, slash)
  const prefix = parseInt(cidr.slice(slash + 1), 10)
  if (isNaN(prefix) || prefix < 0 || prefix > 32) return null
  const parts = net.split(".").map(Number)
  if (parts.length !== 4) return null
  if (parts.some((p) => Number.isNaN(p) || p < 0 || p > 255)) return null
  const base =
    ((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]) >>> 0
  const total = 2 ** (32 - prefix)
  if (total < 4) return null // /31, /32 don't have a usable peer range
  const firstU32 = (base + 2) >>> 0
  const lastU32 = (base + total - 2) >>> 0
  const u32ToIp = (n: number) =>
    `${(n >>> 24) & 0xff}.${(n >>> 16) & 0xff}.${(n >>> 8) & 0xff}.${n & 0xff}`
  return { first: u32ToIp(firstU32), last: u32ToIp(lastU32), total: total - 3 }
}
