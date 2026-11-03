// `pnpm qa:shots` (M2-E4-T2, docs/07 section 12): one deterministic PNG per matrix cell on the
// qa build, plus the emulation self-test. Output: qa/out/shots/<run>/<screen>/<cell>.png and
// `.cells/*.json` (read by the contact-sheet global teardown).
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { expect, test } from "@playwright/test";
import {
  applyEmulation,
  diffEmulation,
  expectedEmulation,
  pngHeaderSize,
  readEmulation,
} from "./emulation";
import {
  buildMatrix,
  cellFile,
  cellName,
  cellUrl,
  pngSize,
  type Cell,
  type Subset,
} from "./matrix";

/** The fixed clock of every still (the fake bridge stamps events with `new Date()`). */
const FIXED_TIME = new Date("2026-03-14T12:00:00.000Z");
/** Neutralises animations and carets in the page; Playwright's `animations: "disabled"` does the rest. */
const FREEZE_CSS =
  "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}";

const RUN_DIR = process.env.QA_SHOTS_DIR ?? path.resolve("qa/out/shots/manual");
const SUBSET: Subset = process.env.QA_SHOTS_SUBSET === "ci" ? "ci" : "full";

async function prepare(page: import("@playwright/test").Page, cell: Cell): Promise<void> {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.goto(cellUrl(cell));
  await applyEmulation(page, cell);
  await page.addStyleTag({ content: FREEZE_CSS });
  await page.locator("#root > *").first().waitFor();
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          resolve();
        });
      });
    });
  });
  // Lazy chunk + first data from the fake bridge (no latency in these scenarios).
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(400);
}

/**
 * Screenshots until two consecutive frames are byte-identical (max 12 tries, 150 ms apart): a
 * late layout pass or font swap at fractional DPR otherwise leaves a 1-pixel difference between
 * runs (seen at 1.25x).
 */
async function stableShot(page: import("@playwright/test").Page): Promise<Buffer> {
  const options = { animations: "disabled", caret: "hide", scale: "device" } as const;
  let previous = await page.screenshot(options);
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await page.waitForTimeout(150);
    const next = await page.screenshot(options);
    if (next.equals(previous)) return next;
    previous = next;
  }
  throw new Error("the screen never settled: two consecutive stills kept differing");
}

async function newCellPage(browser: import("@playwright/test").Browser, cell: Cell) {
  const context = await browser.newContext({
    viewport: { width: cell.size.w, height: cell.size.h },
    deviceScaleFactor: cell.dpr,
    locale: cell.locale === "en-US" ? "en-US" : "pt-BR",
    timezoneId: "UTC",
    colorScheme: cell.theme,
    reducedMotion: cell.motion === "reduced" ? "reduce" : "no-preference",
    forcedColors: cell.forced ? "active" : "none",
  });
  return { context, page: await context.newPage() };
}

test.describe("screenshot matrix", () => {
  for (const cell of buildMatrix(SUBSET)) {
    const title = `${cell.screen.id} ${cellName(cell)}${cell.informative ? " [informative]" : ""}`;
    test(title, async ({ browser }) => {
      const { context, page } = await newCellPage(browser, cell);
      try {
        const errors: string[] = [];
        page.on("pageerror", (error) => errors.push(error.message));
        await prepare(page, cell);

        // Emulation self-check per cell: dark, reduced motion, reduced transparency, DPR.
        expect(diffEmulation(await readEmulation(page), expectedEmulation(cell))).toEqual([]);

        const file = path.join(RUN_DIR, cellFile(cell));
        mkdirSync(path.dirname(file), { recursive: true });
        const png = await stableShot(page);
        writeFileSync(file, png);

        // PNG size = viewport x DPR, read from the header.
        expect(pngHeaderSize(png)).toEqual(pngSize(cell));
        expect(errors, "page errors").toEqual([]);

        const cellsDir = path.join(RUN_DIR, ".cells");
        mkdirSync(cellsDir, { recursive: true });
        writeFileSync(
          path.join(cellsDir, `${cell.screen.id}__${cellName(cell)}.json`),
          JSON.stringify({
            screen: cell.screen.id,
            file: cellFile(cell),
            name: cellName(cell),
            informative: cell.informative,
            width: pngSize(cell).w,
            height: pngSize(cell).h,
            sha256: createHash("sha256").update(png).digest("hex"),
          }),
        );
      } finally {
        await context.close();
      }
    });
  }
});

test.describe("emulation self-test", () => {
  const probe = buildMatrix("full").filter(
    (c) =>
      c.screen.id === "dashboard" &&
      c.size.w === 1280 &&
      (c.theme === "dark" || c.motion === "reduced" || c.vibrancy === "off" || c.dpr !== 1),
  );

  for (const cell of probe) {
    test(`reports ${cellName(cell)}`, async ({ browser }) => {
      const { context, page } = await newCellPage(browser, cell);
      try {
        await page.goto("/");
        await applyEmulation(page, cell);
        const state = await readEmulation(page);
        expect(state.dark).toBe(cell.theme === "dark");
        expect(state.reducedMotion).toBe(cell.motion === "reduced");
        expect(state.reducedTransparency).toBe(cell.vibrancy === "off");
        expect(state.dpr).toBe(cell.dpr);
      } finally {
        await context.close();
      }
    });
  }

  test("a wrong emulation is detected (negative case)", async ({ browser }) => {
    const [cell] = buildMatrix("full");
    if (cell === undefined) throw new Error("empty matrix");
    const { context, page } = await newCellPage(browser, cell);
    try {
      await page.goto("/");
      await applyEmulation(page, cell);
      const wrong = { ...expectedEmulation(cell), dark: true, dpr: 3, reducedTransparency: true };
      expect(diffEmulation(await readEmulation(page), wrong)).toHaveLength(3);
    } finally {
      await context.close();
    }
  });

  test("the PNG header check reads the size and rejects other data", () => {
    const png = Buffer.alloc(24);
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(png);
    png.writeUInt32BE(1600, 16);
    png.writeUInt32BE(1000, 20);
    expect(pngHeaderSize(png)).toEqual({ w: 1600, h: 1000 });
    expect(() => pngHeaderSize(Buffer.from("not a png at all, really not"))).toThrow();
  });
});
