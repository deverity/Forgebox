// @vitest-environment node
// The coefficient-of-variation arithmetic of `pnpm qa:perf` (scripts/qa-perf.mjs).
import { describe, expect, it } from "vitest";
import { summarize } from "../../scripts/qa-perf.mjs";

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
