// Per-cell browser emulation of `pnpm qa:shots` and its self-test (M2-E4-T2). One CDP call sets
// every media feature, because Emulation.setEmulatedMedia replaces the whole feature list.
import type { Page } from "@playwright/test";
import type { Cell } from "./matrix";

export interface EmulationState {
  dark: boolean;
  reducedMotion: boolean;
  reducedTransparency: boolean;
  forcedColors: boolean;
  dpr: number;
  innerWidth: number;
  innerHeight: number;
}

/** What the page must report for a cell: vibrancy off doubles as prefers-reduced-transparency. */
export function expectedEmulation(cell: Cell): EmulationState {
  return {
    dark: cell.theme === "dark",
    reducedMotion: cell.motion === "reduced",
    reducedTransparency: cell.vibrancy === "off",
    forcedColors: cell.forced,
    dpr: cell.dpr,
    innerWidth: cell.size.w,
    innerHeight: cell.size.h,
  };
}

/** Applies the cell's media features through CDP (the context already has viewport and DPR). */
export async function applyEmulation(page: Page, cell: Cell): Promise<void> {
  const session = await page.context().newCDPSession(page);
  await session.send("Emulation.setEmulatedMedia", {
    features: [
      { name: "prefers-color-scheme", value: cell.theme },
      {
        name: "prefers-reduced-motion",
        value: cell.motion === "reduced" ? "reduce" : "no-preference",
      },
      {
        name: "prefers-reduced-transparency",
        value: cell.vibrancy === "off" ? "reduce" : "no-preference",
      },
      { name: "forced-colors", value: cell.forced ? "active" : "none" },
    ],
  });
}

/** Reads the emulation back from the page. */
export function readEmulation(page: Page): Promise<EmulationState> {
  return page.evaluate(() => ({
    dark: matchMedia("(prefers-color-scheme: dark)").matches,
    reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
    reducedTransparency: matchMedia("(prefers-reduced-transparency: reduce)").matches,
    forcedColors: matchMedia("(forced-colors: active)").matches,
    dpr: window.devicePixelRatio,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
  }));
}

/** Differences between two states, empty when they match. */
export function diffEmulation(actual: EmulationState, expected: EmulationState): string[] {
  const out: string[] = [];
  for (const key of Object.keys(expected) as (keyof EmulationState)[]) {
    if (actual[key] !== expected[key]) {
      out.push(`${key}: expected ${String(expected[key])}, got ${String(actual[key])}`);
    }
  }
  return out;
}

/** Width and height from the PNG IHDR chunk (bytes 16..24); throws on a non-PNG buffer. */
export function pngHeaderSize(png: Buffer): { w: number; h: number } {
  const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
  if (png.length < 24 || signature.some((byte, i) => png[i] !== byte)) {
    throw new Error("not a PNG");
  }
  return { w: png.readUInt32BE(16), h: png.readUInt32BE(20) };
}
