import { useVirtualizer } from "@tanstack/react-virtual";
import { type ReactNode, useRef } from "react";

export interface VirtualListProps<T> {
  items: readonly T[];
  /** Fixed row height in px (ListRow is 48): no measuring, so scrolling stays cheap. */
  rowHeight?: number;
  /** Stable identity per item (never the index) so rows keep their state while scrolling. */
  getKey: (item: T, index: number) => string;
  renderRow: (item: T, index: number) => ReactNode;
  overscan?: number;
  /** Viewport height used before the first layout pass (SSR/jsdom) so the first paint has rows. */
  initialHeight?: number;
  className?: string;
  /** Accessible name of the scroll region. */
  label: string;
}

/**
 * Windowed list (docs/07 section 10: 60 fps with 500 items). Only the visible rows plus
 * `overscan` are mounted; the container must be given a bounded height by the caller.
 */
export function VirtualList<T>({
  items,
  rowHeight = 48,
  getKey,
  renderRow,
  overscan = 8,
  initialHeight = 480,
  className,
  label,
}: VirtualListProps<T>) {
  const parentRef = useRef<HTMLDivElement>(null);
  // TanStack's virtualizer returns unmemoizable functions; this component is deliberately not
  // compiled by React Compiler and does not pass them to memoized children.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => rowHeight,
    overscan,
    initialRect: { width: 0, height: initialHeight },
    getItemKey: (index) => {
      const item = items[index];
      return item === undefined ? index : getKey(item, index);
    },
  });

  return (
    <div
      ref={parentRef}
      role="list"
      aria-label={label}
      className={["h-full overflow-auto", className].filter(Boolean).join(" ")}
    >
      <div style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%" }}>
        {virtualizer.getVirtualItems().map((row) => {
          const item = items[row.index];
          if (item === undefined) return null;
          return (
            <div
              key={row.key}
              role="listitem"
              aria-posinset={row.index + 1}
              aria-setsize={items.length}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                height: row.size,
                transform: `translateY(${String(row.start)}px)`,
              }}
            >
              {renderRow(item, row.index)}
            </div>
          );
        })}
      </div>
    </div>
  );
}
