import type { Icon } from "@tabler/icons-react"

interface TileOption<T extends string> {
  value: T
  label: string
  Icon: Icon
}

/**
 * Inline single-select as a grid of icon tiles — a robust replacement for a
 * dropdown `Select` when the option set is small and fixed (device OS / type).
 *
 * Plain `<button>`s in a `radiogroup`: no portal, no floating layer, no focus
 * trap, no `pointer-events` body lock — so it can't get into the "dropdown
 * won't open inside a Dialog/Sheet" state that Radix Select can. Keyboard- and
 * screen-reader-friendly via `role=radio` + `aria-checked`.
 *
 * `value === ""` renders with nothing selected (the create flow requires a
 * deliberate pick).
 */
export function OptionTiles<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
}: {
  options: readonly TileOption<T>[]
  value: T | ""
  onChange: (v: T) => void
  ariaLabel?: string
}) {
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className="grid gap-1.5"
      style={{ gridTemplateColumns: "repeat(auto-fit, minmax(74px, 1fr))" }}
    >
      {options.map(({ value: v, label, Icon }) => {
        const selected = v === value
        return (
          <button
            key={v}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(v)}
            className={[
              "flex flex-col items-center justify-center gap-1.5 rounded-md border px-2 py-2.5 text-center transition-colors",
              selected
                ? "border-primary bg-primary text-primary-foreground shadow-[inset_0_0_0_1px_var(--primary)]"
                : "border-border text-muted-foreground hover:border-foreground/40 hover:text-foreground",
            ].join(" ")}
          >
            <Icon className="size-4 shrink-0" />
            <span className="font-mono text-[11px] leading-none">{label}</span>
          </button>
        )
      })}
    </div>
  )
}
