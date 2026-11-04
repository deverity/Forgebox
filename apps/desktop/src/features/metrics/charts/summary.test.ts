import { describe, expect, it } from "vitest";
import { aligned, lastValue, peakValue, summarize, sumColumns } from "./summary";

describe("chart summaries", () => {
  it("takes the latest non-gap value and the peak", () => {
    expect(lastValue([1, 2, null])).toBe(2);
    expect(peakValue([1, 9, null, 3])).toBe(9);
    expect(lastValue([])).toBeNull();
    expect(peakValue([null])).toBeNull();
  });

  it("formats a row and shows a dash for no data", () => {
    const fmt = (v: number) => `${String(v)} ms`;
    expect(summarize("p99", [10, 40, 20], fmt)).toEqual({
      label: "p99",
      latest: "20 ms",
      peak: "40 ms",
    });
    expect(summarize("p50", [null, null], fmt)).toEqual({ label: "p50", latest: "-", peak: "-" });
  });

  it("stacks columns, keeping a gap only when both are gaps", () => {
    expect(sumColumns([1, null, null], [2, 3, null])).toEqual([3, 3, null]);
  });

  it("builds aligned data with the time column first", () => {
    expect(aligned([1, 2], [3, 4])).toEqual([
      [1, 2],
      [3, 4],
    ]);
  });
});
