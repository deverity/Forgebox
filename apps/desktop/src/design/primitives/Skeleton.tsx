import type { ReactNode } from "react";

export interface SkeletonProps {
  /** Fixed height in px: a skeleton never sizes itself, so nothing shifts when data arrives. */
  height: number;
  width?: number | string;
  className?: string;
}

/** Placeholder block. Pulse is CSS and only runs when the user allows motion. */
export function Skeleton({ height, width = "100%", className }: SkeletonProps) {
  return (
    <div
      aria-hidden="true"
      data-skeleton=""
      style={{ height, width }}
      className={["rounded-control bg-stroke motion-safe:animate-pulse", className]
        .filter(Boolean)
        .join(" ")}
    />
  );
}

export interface SkeletonSlotProps {
  loading: boolean;
  /** Height of the slot in both states; the content must fit in it. */
  height: number;
  /** Accessible loading text, announced while loading. */
  label: string;
  children: ReactNode;
  className?: string;
}

/**
 * Reserves a fixed-height slot and swaps skeleton for content inside it, so switching from
 * loading to loaded never changes the layout (docs/07 section 11, "no layout shift").
 */
export function SkeletonSlot({ loading, height, label, children, className }: SkeletonSlotProps) {
  return (
    <div
      data-skeleton-slot=""
      aria-busy={loading}
      style={{ height }}
      className={["overflow-hidden", className].filter(Boolean).join(" ")}
    >
      {loading ? (
        <>
          <span className="sr-only">{label}</span>
          <Skeleton height={height} />
        </>
      ) : (
        children
      )}
    </div>
  );
}
