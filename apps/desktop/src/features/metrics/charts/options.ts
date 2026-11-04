import type uPlot from "uplot";
import { AXIS_FONT_PX, type ChartPalette } from "./palette";

/**
 * Memoises a tick formatter. Axis ticks are "nice" numbers that repeat from one update to the
 * next, while building an `Intl.NumberFormat` per call is the costliest part of an axis redraw
 * (the update budget is 4 ms, docs/07 section 10).
 */
export function memoFormat(format: (value: number) => string): (value: number) => string {
  const cache = new Map<number, string>();
  return (value) => {
    let text = cache.get(value);
    if (text === undefined) {
      if (cache.size >= 512) cache.clear();
      text = format(value);
      cache.set(value, text);
    }
    return text;
  };
}

/** Shared uPlot options: no legend, no cursor chrome, token-coloured axes and grid. */
export function baseOptions(
  p: ChartPalette,
  locale: string,
  extraAxes: uPlot.Axis[] = [],
  yFormat?: (v: number) => string,
): Pick<uPlot.Options, "legend" | "cursor" | "select" | "axes" | "padding"> {
  const format = yFormat ? memoFormat(yFormat) : undefined;
  const font = `${String(AXIS_FONT_PX)}px ${p.fontFamily}`;
  const time = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" });
  const axis = {
    stroke: p.axis,
    font,
    grid: { stroke: p.grid, width: 1 },
    ticks: { stroke: p.grid, width: 1 },
  };
  return {
    legend: { show: false },
    cursor: { show: false },
    select: { show: false, left: 0, top: 0, width: 0, height: 0 },
    padding: [8, 8, 0, 0],
    axes: [
      {
        ...axis,
        values: (_u, splits) => splits.map((v) => time.format(v * 1000)),
      },
      {
        ...axis,
        size: 56,
        ...(format ? { values: (_u: uPlot, splits: number[]) => splits.map(format) } : {}),
      },
      ...extraAxes,
    ],
  };
}
