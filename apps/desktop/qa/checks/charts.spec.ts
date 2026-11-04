// Metrics charts (M2-E5-T15) on the real engine: states, themes, DPR, markers, update cost.
// Stills go to qa/out/charts/ (gitignored); they are inspected by eye, the assertions below
// are what gates.
import { mkdirSync } from "node:fs";
import path from "node:path";
import type { Page } from "@playwright/test";
import { emulate, expect, routeUrl, test } from "./lib";

const SHOTS = path.resolve(import.meta.dirname, "../out/charts");
const READY = "[data-chart-state=ready] [data-chart-canvas] canvas";

async function open(
  page: Page,
  query: Record<string, string>,
  options: Parameters<typeof routeUrl>[1] = {},
): Promise<void> {
  await page.goto(routeUrl("/__charts", options, query));
  await page.locator("[data-charts-harness]").waitFor();
}

async function waitDrawn(page: Page): Promise<void> {
  await page.locator(READY).first().waitFor({ timeout: 15_000 });
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            resolve();
          });
        });
      }),
  );
}

/** Canvas backing size against its CSS size, and whether anything was drawn. */
async function canvasFacts(page: Page) {
  return page.evaluate(() =>
    [...document.querySelectorAll<HTMLCanvasElement>("[data-chart-canvas] canvas")].map((c) => {
      const ctx = c.getContext("2d");
      const px = ctx ? ctx.getImageData(0, 0, c.width, c.height).data : new Uint8ClampedArray();
      let painted = 0;
      for (let i = 3; i < px.length; i += 4) if ((px[i] ?? 0) > 0) painted += 1;
      return {
        width: c.width,
        cssWidth: c.getBoundingClientRect().width,
        dpr: window.devicePixelRatio,
        painted,
      };
    }),
  );
}

const MODES = [
  { name: "light", theme: "light", vibrancy: "on" },
  { name: "dark", theme: "dark", vibrancy: "on" },
  { name: "light-vibrancy-off", theme: "light", vibrancy: "off" },
  { name: "dark-vibrancy-off", theme: "dark", vibrancy: "off" },
] as const;

for (const mode of MODES) {
  for (const dpr of [1, 1.25, 1.5]) {
    test(`charts render crisp at DPR ${String(dpr)}: ${mode.name}`, async ({ page }) => {
      await emulate(page, { colorScheme: mode.theme, deviceScaleFactor: dpr });
      await open(page, { points: "300" }, { theme: mode.theme, vibrancy: mode.vibrancy });
      await waitDrawn(page);
      const facts = await canvasFacts(page);
      expect(facts.length).toBe(4);
      for (const c of facts) {
        expect(c.dpr).toBe(dpr);
        expect(Math.abs(c.width - c.cssWidth * dpr)).toBeLessThanOrEqual(1);
        expect(c.painted).toBeGreaterThan(0);
      }
      mkdirSync(SHOTS, { recursive: true });
      await page.screenshot({
        path: path.join(SHOTS, `${mode.name}-${String(dpr)}x.png`),
        animations: "disabled",
        fullPage: true,
      });
    });
  }
}

test("re-themes in place when the theme attribute changes", async ({ page }) => {
  await open(page, { points: "300", only: "latency" }, { theme: "light", vibrancy: "off" });
  await waitDrawn(page);
  const grab = () =>
    page.evaluate(() =>
      document.querySelector<HTMLCanvasElement>("[data-chart-canvas] canvas")?.toDataURL(),
    );
  const light = await grab();
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "dark";
  });
  await expect.poll(grab).not.toBe(light);
});

test("loading shows a skeleton and empty an empty state, both at the chart height", async ({
  page,
}) => {
  await open(page, { state: "loading" });
  await page.locator("[data-chart-state=loading]").first().waitFor();
  const loadingHeights = await page
    .locator("[data-skeleton-slot]")
    .evaluateAll((els) => els.map((e) => e.getBoundingClientRect().height));
  expect(loadingHeights).toEqual([200, 200, 200]);
  await expect(page.locator("[data-chart-state=loading]")).toHaveCount(3);

  await open(page, { state: "empty" });
  await page.locator("[data-chart-state=empty]").first().waitFor();
  await expect(page.locator("[data-chart-state=empty]")).toHaveCount(3);
  const emptyHeights = await page
    .locator("[data-skeleton-slot]")
    .evaluateAll((els) => els.map((e) => e.getBoundingClientRect().height));
  expect(emptyHeights).toEqual([200, 200, 200]);
});

test("markers are reachable by Tab and show a tooltip on focus", async ({ page }) => {
  await open(page, { points: "300", only: "latency" }, { theme: "light", vibrancy: "off" });
  await waitDrawn(page);
  const kinds: string[] = [];
  for (let i = 0; i < 4; i += 1) {
    await page.keyboard.press("Tab");
    kinds.push(
      await page.evaluate(() => document.activeElement?.getAttribute("data-marker-kind") ?? "none"),
    );
  }
  expect(kinds).toEqual(["php-version", "restart", "config", "update"]);
  await expect(page.locator("[role=tooltip][data-state*=open]")).toHaveCount(1);
});

test("the aria summary table lists the percentiles", async ({ page }) => {
  await open(page, { points: "300", only: "latency" }, { locale: "en-US" });
  await waitDrawn(page);
  const rows = await page
    .locator("[data-chart-summary] tbody th")
    .evaluateAll((els) => els.map((e) => e.textContent));
  expect(rows).toEqual(["p50", "p90", "p99", "PHP time"]);
});

test("reduced motion: marker handles do not transition", async ({ page }) => {
  await emulate(page, { reducedMotion: true });
  await open(page, { points: "300", only: "latency" }, { motion: "reduced" });
  await waitDrawn(page);
  const duration = await page
    .locator("[data-marker-kind]")
    .first()
    .evaluate((el) => getComputedStyle(el).transitionDuration);
  expect(duration).toBe("0s");
});
