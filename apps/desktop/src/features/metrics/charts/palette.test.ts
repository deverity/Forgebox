// Readable bands (M2-E4-T10, docs/07 section 11 item 4): the translucent p90 and p99 bands of
// the latency chart stay distinguishable from the surface in the light and the dark theme.
import { describe, expect, it } from "vitest";
import { colors } from "../../../design/tokens";
import { BAND_ALPHA } from "./palette";

type Rgb = readonly [number, number, number];

function rgb(hex: string): Rgb {
  const n = (i: number) => Number.parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16);
  return [n(0), n(1), n(2)];
}

function over(fill: Rgb, alpha: number, bg: Rgb): Rgb {
  return [0, 1, 2].map(
    (i) => (fill[i] ?? 0) * alpha + (bg[i] ?? 0) * (1 - alpha),
  ) as unknown as Rgb;
}

function luminance([r, g, b]: Rgb): number {
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

function ratio(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

describe("latency band fills", () => {
  for (const theme of ["light", "dark"] as const) {
    for (const [band, token] of [
      ["p99 (danger)", "state-failed"],
      ["p90 (warn)", "state-starting"],
    ] as const) {
      it(`${theme}: the ${band} band is at least 1.3:1 against the surface`, () => {
        const surface = rgb(colors[theme]["bg-surface"]);
        const fill = over(rgb(colors[theme][token]), BAND_ALPHA / 100, surface);
        expect(ratio(fill, surface)).toBeGreaterThanOrEqual(1.3);
      });
    }
  }

  it("keeps the two bands apart from each other (different hue, not just different shade)", () => {
    for (const theme of ["light", "dark"] as const) {
      const surface = rgb(colors[theme]["bg-surface"]);
      const a = over(rgb(colors[theme]["state-failed"]), BAND_ALPHA / 100, surface);
      const b = over(rgb(colors[theme]["state-starting"]), BAND_ALPHA / 100, surface);
      const spread = Math.max(...a.map((c, i) => Math.abs(c - (b[i] ?? 0))));
      expect(spread, theme).toBeGreaterThan(8);
    }
  });
});
