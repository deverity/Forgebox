// @vitest-environment node
// The statistics `pnpm perf:startup` reports (scripts/qa-root.mjs).
import { describe, expect, it } from "vitest";
import { median, percentile } from "../../scripts/qa-root.mjs";

describe("perf-startup statistics", () => {
  it("median of an even count is the mean of the two middle values", () => {
    expect(median([5, 1, 3, 2])).toBe(2.5);
    expect(median([9, 1, 5])).toBe(5);
    expect(median([])).toBeNull();
  });

  it("percentiles use the nearest rank", () => {
    const ten = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    expect(percentile(ten, 0.9)).toBe(90);
    expect(percentile(ten, 0.95)).toBe(100);
    expect(percentile([7], 0.95)).toBe(7);
  });
});
