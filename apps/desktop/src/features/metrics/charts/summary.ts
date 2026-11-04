import type uPlot from "uplot";
import { NO_VALUE } from "../format";
import type { Column, SummaryRow } from "./types";

export function lastValue(column: Column): number | null {
  for (let i = column.length - 1; i >= 0; i -= 1) {
    const v = column[i];
    if (v !== null && v !== undefined && Number.isFinite(v)) return v;
  }
  return null;
}

export function peakValue(column: Column): number | null {
  let peak: number | null = null;
  for (const v of column) {
    if (v !== null && Number.isFinite(v) && (peak === null || v > peak)) peak = v;
  }
  return peak;
}

/** Latest and peak of a column, formatted; a dash stands for "no data". */
export function summarize(
  label: string,
  column: Column,
  format: (value: number) => string,
  swatch?: string,
): SummaryRow {
  const latest = lastValue(column);
  const peak = peakValue(column);
  return {
    label,
    ...(swatch === undefined ? {} : { swatch }),
    latest: latest === null ? NO_VALUE : format(latest),
    peak: peak === null ? NO_VALUE : format(peak),
  };
}

/** Per-point sum of two columns (stacking); a gap in both stays a gap. */
export function sumColumns(a: Column, b: Column): (number | null)[] {
  return a.map((v, i) => {
    const w = b[i] ?? null;
    if (v === null && w === null) return null;
    return (v ?? 0) + (w ?? 0);
  });
}

/** Builds uPlot aligned data from readonly columns (uPlot never mutates its input). */
export function aligned(t: readonly number[], ...columns: Column[]): uPlot.AlignedData {
  return [t, ...columns] as unknown as uPlot.AlignedData;
}
