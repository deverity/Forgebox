import { motion } from "motion/react";
import { Progress as RadixProgress } from "radix-ui";
import { useTranslation } from "react-i18next";
import { TNUM_CLASS } from "../../i18n/format";
import { resolveMotion } from "../motion";
import { useFormatters } from "./useFormatters";

export interface ProgressProps {
  /** 0..100, or null for indeterminate. 100 (or `done`) morphs the bar into a check. */
  value: number | null;
  variant?: "linear" | "circular";
  /** Accessible name, e.g. "Downloading PHP 8.4". */
  label: string;
  /** Bytes per second; shown as "12 MB/s". */
  speedBps?: number;
  etaSeconds?: number;
  done?: boolean;
  className?: string;
}

const RING_RADIUS = 18;

// SVG presentation attributes live in constants: the no-literal-string rule would flag them inline.
const CHECK_SHAPE = {
  d: "M7 13l4 4 8-9",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2.5,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;
const RING_SHAPE = { cx: 20, cy: 20, r: RING_RADIUS, fill: "none", strokeWidth: 3 } as const;
const RING_VIEWBOX = "0 0 40 40";
const CHECK_VIEWBOX = "0 0 26 26";
const PART_INDICATOR = { "data-part": "indicator" } as const;

function CheckPath({ draw }: { draw: boolean }) {
  const preset = resolveMotion("transition");
  return (
    <motion.path
      {...CHECK_SHAPE}
      initial={draw ? { pathLength: 0, opacity: 1 } : { opacity: 0 }}
      animate={draw ? { pathLength: 1, opacity: 1 } : { opacity: 1 }}
      transition={preset.transition}
    />
  );
}

/** Linear or circular progress with speed/ETA text; on completion the bar morphs to a check. */
export function Progress({
  value,
  variant = "linear",
  label,
  speedBps,
  etaSeconds,
  done,
  className,
}: ProgressProps) {
  const { t } = useTranslation();
  const f = useFormatters();
  const isDone = done ?? (value !== null && value >= 100);
  const clamped = value === null ? null : Math.min(100, Math.max(0, value));
  const fadeOnly = resolveMotion("transition").fadeOnly;
  const transition = resolveMotion("transition").transition;
  const fraction = isDone ? 1 : (clamped ?? 0) / 100;

  const parts: string[] = [];
  if (isDone) {
    parts.push(t("ui.progressDone"));
  } else {
    if (clamped !== null) parts.push(f.percent(clamped / 100, 0));
    if (speedBps !== undefined) parts.push(t("ui.progressSpeed", { speed: f.bytes(speedBps) }));
    if (etaSeconds !== undefined)
      parts.push(t("ui.progressEta", { eta: f.duration(etaSeconds * 1000) }));
  }
  const text = parts.join(" · ");

  const state = isDone ? "done" : clamped === null ? "indeterminate" : "loading";

  return (
    <div
      data-state={state}
      data-variant={variant}
      className={[
        variant === "circular" ? "inline-flex flex-col items-center gap-1" : "flex flex-col gap-1",
        className,
      ]
        .filter(Boolean)
        .join(" ")}
    >
      <RadixProgress.Root
        value={isDone ? 100 : clamped}
        max={100}
        aria-label={label}
        aria-valuetext={text || undefined}
        className={variant === "circular" ? "relative size-10" : "relative h-1.5 w-full"}
      >
        {variant === "linear" ? (
          <div className="relative h-full w-full overflow-hidden rounded-full bg-stroke">
            <motion.div
              {...PART_INDICATOR}
              className={[
                "h-full w-full origin-left rounded-full",
                isDone ? "bg-state-running" : "bg-accent",
              ].join(" ")}
              initial={false}
              animate={
                clamped === null && !isDone
                  ? { scaleX: 0.3, opacity: [0.4, 1, 0.4] }
                  : { scaleX: fraction, opacity: 1 }
              }
              transition={
                clamped === null && !isDone
                  ? { duration: 1.2, repeat: fadeOnly ? 0 : Infinity }
                  : transition
              }
            />
          </div>
        ) : (
          <svg viewBox={RING_VIEWBOX} className="size-10 -rotate-90" aria-hidden="true">
            <circle {...RING_SHAPE} className="stroke-stroke" />
            <motion.circle
              {...RING_SHAPE}
              strokeLinecap="round"
              className={isDone ? "stroke-state-running" : "stroke-accent"}
              initial={false}
              animate={{ pathLength: fraction, opacity: 1 }}
              transition={transition}
            />
          </svg>
        )}
        {isDone ? (
          <span
            data-part="check"
            className={[
              "text-state-running",
              variant === "circular"
                ? "absolute inset-0 flex items-center justify-center"
                : "absolute -top-5 right-0",
            ].join(" ")}
          >
            <svg viewBox={CHECK_VIEWBOX} className="size-4" aria-hidden="true">
              <CheckPath draw={!fadeOnly} />
            </svg>
          </span>
        ) : null}
      </RadixProgress.Root>
      {text ? (
        <p className={`text-caption text-fg-secondary ${TNUM_CLASS}`} data-part="text">
          {text}
        </p>
      ) : null}
    </div>
  );
}
