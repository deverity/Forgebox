import type { ReactNode } from "react";
import { cx } from "./shared";

export type BadgeTone = "neutral" | "accent" | "success" | "warning" | "danger" | "info";

export const BADGE_TONES: readonly BadgeTone[] = [
  "neutral",
  "accent",
  "success",
  "warning",
  "danger",
  "info",
];

// The tone is carried by the border only: state-* tokens are guaranteed 3:1 (UI), not the 4.5:1
// that small text needs (docs/07 section 2.3), so the text stays fg-primary/fg-secondary.
const TONE: Record<BadgeTone, string> = {
  neutral: "text-fg-secondary border-stroke-strong",
  accent: "text-fg-primary border-accent",
  success: "text-fg-primary border-state-running",
  warning: "text-fg-primary border-state-starting",
  danger: "text-fg-primary border-state-failed",
  info: "text-fg-primary border-state-updating",
};

/** Small status/count label. The meaning is always in the text, never only in the colour. */
export function Badge({ tone = "neutral", children }: { tone?: BadgeTone; children: ReactNode }) {
  return (
    <span
      data-tone={tone}
      className={cx(
        "inline-flex h-5 items-center rounded-full border bg-surface-2 px-2 text-caption tabular-nums",
        TONE[tone],
      )}
    >
      {children}
    </span>
  );
}
