import { memo, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { TNUM_CLASS } from "../../i18n/format";
import { useFormatters } from "./useFormatters";

/** Samples kept per sparkline (docs/07 section 10: one point per second for a minute). */
export const SPARKLINE_SAMPLES = 60;

export interface SparklineProps {
  samples: readonly number[];
  width?: number;
  height?: number;
  /** Fixed upper bound (e.g. 100 for percentages); defaults to the window's peak. */
  max?: number;
  /** Formats the latest/peak labels; defaults to the locale number format. */
  format?: (value: number) => string;
  showLabels?: boolean;
  className?: string;
}

const PAD = 2;

export function sparklinePoints(
  samples: readonly number[],
  width: number,
  height: number,
  max: number,
): string {
  const window = samples.slice(-SPARKLINE_SAMPLES);
  if (window.length === 0) return "";
  const top = max > 0 ? max : 1;
  const step = window.length > 1 ? (width - PAD * 2) / (window.length - 1) : 0;
  return window
    .map((v, i) => {
      const x = PAD + i * step;
      const y = height - PAD - (Math.min(Math.max(v, 0), top) / top) * (height - PAD * 2);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
}

function SparklineView({
  samples,
  width = 120,
  height = 32,
  max,
  format,
  showLabels = true,
  className,
}: SparklineProps) {
  const { t } = useTranslation();
  const f = useFormatters();
  const fmt = format ?? ((v: number) => f.number(v, { maximumFractionDigits: 1 }));
  const window = useMemo(() => samples.slice(-SPARKLINE_SAMPLES), [samples]);
  const peak = window.length > 0 ? Math.max(...window) : 0;
  const latest = window.at(-1) ?? 0;
  const points = useMemo(
    () => sparklinePoints(window, width, height, max ?? peak),
    [window, width, height, max, peak],
  );
  const summary = t("ui.sparklineLabel", {
    count: window.length,
    latest: fmt(latest),
    max: fmt(peak),
  });

  return (
    <div className={["inline-flex items-center gap-2", className].filter(Boolean).join(" ")}>
      <svg
        role="img"
        aria-label={summary}
        width={width}
        height={height}
        viewBox={`0 0 ${String(width)} ${String(height)}`}
        className="shrink-0 text-accent"
      >
        <polyline
          points={points}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
        />
      </svg>
      {showLabels ? (
        <span className={`text-caption text-fg-secondary ${TNUM_CLASS}`} data-part="labels">
          <span data-part="latest" className="text-fg-primary">
            {fmt(latest)}
          </span>
          <span aria-hidden="true"> / </span>
          <span data-part="peak">{fmt(peak)}</span>
        </span>
      ) : null}
    </div>
  );
}

function sameSamples(a: readonly number[], b: readonly number[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  return a.every((v, i) => v === b[i]);
}

/**
 * Memoised on sample *values*: a parent re-render (or a sibling's tick) with unchanged data does
 * not re-render this sparkline, and a store can safely hand over a fresh array each tick.
 */
export const Sparkline = memo(SparklineView, (prev, next) => {
  const { samples: a, ...restA } = prev;
  const { samples: b, ...restB } = next;
  const keys = new Set([...Object.keys(restA), ...Object.keys(restB)]) as Set<keyof typeof restA>;
  for (const k of keys) if (restA[k] !== restB[k]) return false;
  return sameSamples(a, b);
});
