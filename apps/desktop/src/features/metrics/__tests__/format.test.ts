import { describe, expect, it } from "vitest";
import { NO_VALUE, createMetricsFormatters } from "../format";

const N = " ";

describe("metrics formatters", () => {
  const pt = createMetricsFormatters("pt-BR");
  const en = createMetricsFormatters("en-US");

  it("formats latency in ms and seconds", () => {
    expect(pt.ms(0.4)).toBe(`<1${N}ms`);
    expect(pt.ms(4.25)).toBe(`4,3${N}ms`);
    expect(en.ms(4.25)).toBe(`4.3${N}ms`);
    expect(pt.ms(230.4)).toBe(`230${N}ms`);
    expect(pt.ms(1250)).toBe(`1,3${N}s`);
    expect(en.ms(1250)).toBe(`1.3${N}s`);
    expect(pt.ms(0)).toBe(`0${N}ms`);
  });

  it("formats req/s with the precision of its magnitude", () => {
    expect(pt.rps(0.42)).toBe(`0,42${N}req/s`);
    expect(en.rps(0.42)).toBe(`0.42${N}req/s`);
    expect(pt.rps(3.14)).toBe(`3,1${N}req/s`);
    expect(pt.rps(1250)).toBe(`1.250${N}req/s`);
    expect(en.rps(1250)).toBe(`1,250${N}req/s`);
  });

  it("formats percentages given in percent", () => {
    expect(pt.percent(0.84)).toBe("0,8%");
    expect(en.percent(0.84)).toBe("0.8%");
    expect(pt.percent(42.6)).toBe("43%");
    expect(en.percent(100)).toBe("100%");
  });

  it("formats bytes and counts", () => {
    expect(pt.bytes(1.5 * 1024 * 1024 * 1024)).toBe("1,5 GB");
    expect(en.bytes(1.5 * 1024 * 1024 * 1024)).toBe("1.5 GB");
    expect(pt.count(1234567)).toBe("1.234.567");
    expect(en.count(1234567)).toBe("1,234,567");
  });

  it("prefixes ≈ only for estimates and never for missing values", () => {
    expect(pt.approx(pt.percent(12), true)).toBe(`≈${N}12%`);
    expect(pt.approx(pt.percent(12), false)).toBe("12%");
    expect(pt.approx(pt.percent(12), undefined)).toBe("12%");
    expect(pt.approx(NO_VALUE, true)).toBe(NO_VALUE);
  });

  it("renders missing values as an em dash", () => {
    for (const f of [pt, en]) {
      expect(f.ms(null)).toBe(NO_VALUE);
      expect(f.rps(undefined)).toBe(NO_VALUE);
      expect(f.percent(Number.NaN)).toBe(NO_VALUE);
      expect(f.bytes(null)).toBe(NO_VALUE);
      expect(f.count(null)).toBe(NO_VALUE);
    }
  });

  it("exposes the tabular-figures class", () => {
    expect(pt.tnum).toBe("tabular-nums");
  });
});
