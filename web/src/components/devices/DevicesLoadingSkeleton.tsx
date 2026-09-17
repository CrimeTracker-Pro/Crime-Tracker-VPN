import { Skeleton } from "@/components/ui/skeleton"

export type ViewMode = "list" | "grid"

/** Loading state for the devices panel. Renders the exact table / grid
 *  layout used post-load — same column count + cell heights for the list
 *  view, same card structure (header / rate blocks / chart strip / footer)
 *  for the grid view — so the row-shape doesn't visibly shift when the
 *  query resolves. */
export function DevicesLoadingSkeleton({ viewMode }: { viewMode: ViewMode }) {
  if (viewMode === "list") {
    return (
      <table className="zv-table zv-table-draggable">
        <thead>
          <tr>
            <th className="w-6" aria-label="Drag handle" />
            <th>Name</th>
            <th>OS</th>
            <th>VPN IP</th>
            <th>Allowed IPs</th>
            <th>Status</th>
            <th className="zv-num">TX</th>
            <th className="zv-num">RX</th>
            <th>Last seen</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {Array.from({ length: 6 }).map((_, i) => (
            <tr key={i}>
              <td className="zv-drag-handle">
                <Skeleton className="h-3 w-3 rounded-none" />
              </td>
              <td>
                <div className="flex items-center gap-2">
                  <Skeleton className="size-1.5 rounded-full" />
                  <Skeleton className="h-3 w-32 rounded-none" />
                </div>
              </td>
              <td>
                <Skeleton className="h-3 w-12 rounded-none" />
              </td>
              <td>
                <Skeleton className="h-3 w-20 rounded-none" />
              </td>
              <td>
                <Skeleton className="h-3 w-28 rounded-none" />
              </td>
              <td>
                <Skeleton className="h-4 w-14 rounded-none" />
              </td>
              <td className="zv-num">
                <Skeleton className="ml-auto h-3 w-12 rounded-none" />
              </td>
              <td className="zv-num">
                <Skeleton className="ml-auto h-3 w-12 rounded-none" />
              </td>
              <td>
                <Skeleton className="h-3 w-16 rounded-none" />
              </td>
              <td className="zv-actions">
                <Skeleton className="ml-auto h-3 w-4 rounded-none" />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    )
  }
  return (
    <div className="grid grid-cols-1 gap-3 p-4 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
      {Array.from({ length: 8 }).map((_, i) => (
        <div key={i} className="zv-panel flex flex-col">
          <div className="flex items-start justify-between gap-2 px-4 pt-4 pb-3">
            <div className="flex min-w-0 flex-col gap-1.5">
              <Skeleton className="h-3.5 w-28 rounded-none" />
              <Skeleton className="h-3 w-36 rounded-none" />
            </div>
            <Skeleton className="h-4 w-14 rounded-none" />
          </div>
          <div className="grid grid-cols-2 gap-3 px-4 pb-3">
            <div className="space-y-1">
              <Skeleton className="h-2.5 w-10 rounded-none" />
              <Skeleton className="h-4 w-16 rounded-none" />
            </div>
            <div className="space-y-1">
              <Skeleton className="h-2.5 w-10 rounded-none" />
              <Skeleton className="h-4 w-16 rounded-none" />
            </div>
          </div>
          <div className="-mb-4 px-1">
            <Skeleton className="h-[56px] w-full rounded-none" />
          </div>
          <div className="border-border bg-muted/40 flex items-center justify-between gap-3 border-t px-4 py-2.5">
            <Skeleton className="h-3 w-20 rounded-none" />
            <div className="flex items-center gap-2">
              <Skeleton className="h-3 w-10 rounded-none" />
              <Skeleton className="h-3 w-10 rounded-none" />
            </div>
          </div>
        </div>
      ))}
    </div>
  )
}
