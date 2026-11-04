// @vitest-environment node
// The coefficient-of-variation arithmetic of `pnpm qa:perf` (scripts/qa-perf.mjs).
import { describe, expect, it } from "vitest";
import { percentile, summarize } from "../../scripts/qa-perf.mjs";

describe("qa:perf summary", () => {
  it("computes mean and CV", () => {
    const { mean, cv } = summarize([10, 10, 10]);
    expect(mean).toBe(10);
    expect(cv).toBe(0);
    const noisy = summarize([10, 20, 30]);
    expect(noisy.mean).toBe(20);
    expect(noisy.cv).toBeCloseTo(0.408, 2);
  });

  it("a zero mean has no variation to report", () => {
    expect(summarize([0, 0]).cv).toBe(0);
  });
});

describe("qa:perf percentile (R-24)", () => {
  const sorted = Array.from({ length: 60 }, (_, i) => i + 1);
  it("takes the nearest rank, so one outlier of 60 does not decide the gate", () => {
    expect(percentile(sorted, 0.95)).toBe(57);
    expect(percentile(sorted, 1)).toBe(60);
    expect(percentile([...sorted.slice(0, 59), 900], 0.95)).toBe(57);
  });
  it("is Infinity for no samples, so an empty run fails", () => {
    expect(percentile([], 0.95)).toBe(Number.POSITIVE_INFINITY);
  });
});
