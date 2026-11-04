import type uPlot from "uplot";
import { describe, expect, it } from "vitest";
import { baseOptions, xTimeStyle } from "./options";
import type { ChartPalette } from "./palette";

const palette = { axis: "#000", grid: "#ccc", fontFamily: "sans-serif" } as unknown as ChartPalette;

function ticks(start: number, spanSeconds: number, step: number, locale = "en-US"): string[] {
  const axis = baseOptions(palette, locale).axes?.[0];
  const values = axis?.values as (u: uPlot, splits: number[]) => string[];
  const u = { scales: { x: { min: start, max: start + spanSeconds } } } as unknown as uPlot;
  const splits: number[] = [];
  for (let t = start; t <= start + spanSeconds; t += step) splits.push(t);
  return values(u, splits);
}

describe("x axis tick format", () => {
  it("chooses the style from the visible span", () => {
    expect(xTimeStyle(60)).toBe("seconds");
    expect(xTimeStyle(3600)).toBe("minutes");
    expect(xTimeStyle(7 * 86400)).toBe("days");
  });

  it("60 s series with 2 s ticks: distinct labels with seconds", () => {
    const labels = ticks(1_793_000_000, 60, 2);
    expect(new Set(labels).size).toBe(labels.length);
    for (const l of labels) expect(l).toMatch(/\d{2}:\d{2}:\d{2}/);
  });

  it("1 h ticks keep hour:minute", () => {
    for (const l of ticks(1_793_000_000, 3600, 600)) expect(l).not.toMatch(/\d:\d{2}:\d{2}/);
  });

  it("7 d ticks carry the date", () => {
    const labels = ticks(1_793_000_000, 7 * 86400, 86400);
    expect(new Set(labels).size).toBe(labels.length);
    for (const l of labels) expect(l).toMatch(/[A-Za-z]{3}/);
  });
});
