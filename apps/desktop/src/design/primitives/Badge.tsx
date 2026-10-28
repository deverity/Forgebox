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

const TONE: Record<BadgeTone, string> = {
  neutral: "text-fg-secondary border-stroke-strong",
  accent: "text-accent border-accent",
  success: "text-state-running border-state-running",
  warning: "text-state-starting border-state-starting",
  danger: "text-state-failed border-state-failed",
  info: "text-state-updating border-state-updating",
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
