// /logs in a real browser against the fake bridge (M2-E3-T12): a 1,000 lines/s stream for 10 s
// keeps the main thread free (no long task over 50 ms) and the DOM small (at most 200 rows),
// plus the CLI hint, "load earlier" through before_seq and the remote-daemon rules.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const URL = "/logs?scenario=populated&target=nginx.error";
const FLOOD_MS = 10_000;
const LINES_PER_SECOND = 1000;
const MAX_DOM_ROWS = 200;
const LONG_TASK_MS = 50;

interface Probe {
  longTasks: number[];
  maxRows: number;
  emitted: number;
}

declare global {
  interface Window {
    __logProbe?: Probe;
  }
}

/** Emits the 600 history lines of the scenario (seq 1..600), the domain `logs.read` answers in. */
async function seedHistory(page: Page, target: string) {
  await page.evaluate((id) => {
    const qa = window.__FORGEBOX_QA__;
    if (!qa) throw new Error("QA hooks missing");
    for (let seq = 1; seq <= 600; seq += 1) {
      qa.bridge.emitLog(id, {
        ts: new Date(Date.UTC(2026, 0, 1, 10, 0, 0) + seq * 1000).toISOString(),
        source: "stdout",
        level: seq % 7 === 0 ? "warn" : "info",
        text: `${id} line ${String(seq)}`,
      });
    }
  }, target);
}

test("a 1,000 lines/s stream for 10 s: no long task over 50 ms and at most 200 DOM rows", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.goto(URL);
  const log = page.getByRole("log");
  await expect(log).toBeVisible();

  await page.evaluate(
    ({ ms, perSecond }) => {
      const qa = window.__FORGEBOX_QA__;
      if (!qa) throw new Error("QA hooks missing");
      const probe: Probe = { longTasks: [], maxRows: 0, emitted: 0 };
      window.__logProbe = probe;
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) probe.longTasks.push(entry.duration);
      }).observe({ type: "longtask" });
      const started = performance.now();
      const tickMs = 16;
      const timer = setInterval(() => {
        const elapsed = performance.now() - started;
        const due = Math.floor((Math.min(elapsed, ms) / 1000) * perSecond);
        while (probe.emitted < due) {
          probe.emitted += 1;
          qa.bridge.emitLog("nginx.error", {
            ts: new Date().toISOString(),
            source: "file",
            level: probe.emitted % 50 === 0 ? "error" : "info",
            text: `2026/01/01 10:00:00 [info] ${String(probe.emitted)}#0: *1 upstream sent a response ${"x".repeat(60)}`,
          });
        }
        probe.maxRows = Math.max(probe.maxRows, document.querySelectorAll("[data-row]").length);
        if (elapsed >= ms) clearInterval(timer);
      }, tickMs);
    },
    { ms: FLOOD_MS, perSecond: LINES_PER_SECOND },
  );

  await page.waitForFunction(
    (total) => (window.__logProbe?.emitted ?? 0) >= total,
    (FLOOD_MS / 1000) * LINES_PER_SECOND,
    { timeout: FLOOD_MS + 20_000 },
  );
  await page.waitForTimeout(300);
  const probe = await page.evaluate(() => window.__logProbe);
  expect(probe?.emitted).toBeGreaterThanOrEqual(10_000);
  expect(Math.max(0, ...(probe?.longTasks ?? []))).toBeLessThanOrEqual(LONG_TASK_MS);
  expect(probe?.maxRows ?? 0).toBeGreaterThan(0);
  expect(probe?.maxRows ?? 0).toBeLessThanOrEqual(MAX_DOM_ROWS);
  // Following: the newest line is the one in view.
  await expect(log).toContainText(`${String(probe?.emitted)}#0`);
});

test("shows the equivalent forge logs command, following the filter text", async ({ page }) => {
  await page.goto(URL);
  await expect(page.getByRole("log")).toBeVisible();
  const hint = page.locator("[data-forge-cli]").first();
  await expect(hint).toHaveAttribute("data-forge-cli", "forge logs nginx.error -f");
  await page.getByLabel(/^(Filter|Filtrar)$/).fill("upstream");
  await expect(hint).toHaveAttribute("data-forge-cli", "forge logs nginx.error -f --grep upstream");
});

test("Load earlier pages back with before_seq and puts the lines first", async ({ page }) => {
  await page.goto("/logs?scenario=populated");
  await expect(page.getByTestId("page-logs")).toBeVisible();
  // History first, then follow: the tail of 200 only reaches seq 401..600.
  await seedHistory(page, "nginx.error");
  await page.getByRole("checkbox", { name: "nginx · error" }).check();
  const log = page.getByRole("log");
  await expect(log).toContainText("nginx.error line 600");
  const toTop = () =>
    log.evaluate((el) => {
      el.scrollTop = 0;
    });
  await toTop();
  await expect(log.locator("[data-row]").first()).toContainText("line 401");
  await page.getByRole("button", { name: /Load earlier|Carregar anteriores/ }).click();
  await toTop();
  // One page back (seq 201..400) is now above the tail.
  await expect(log.locator("[data-row]").first()).toContainText("line 201");
  expect(await log.locator("[data-row]").count()).toBeLessThanOrEqual(MAX_DOM_ROWS);
});

test("with a remote daemon file links are plain text with an explanation", async ({ page }) => {
  await page.goto("/logs?scenario=remote&target=php%408.4");
  const log = page.getByRole("log");
  await expect(log).toBeVisible();
  await page.evaluate(() => {
    window.__FORGEBOX_QA__?.bridge.emitLog("php@8.4", {
      ts: null,
      source: "stderr",
      level: "error",
      text: "#0 /var/www/loja/app/Cart.php(87): Cart->total()",
    });
  });
  const plain = log.locator("[data-remote-link]");
  await expect(plain).toHaveAttribute("title", /(daemon|editor)/);
  await expect(log.locator("[data-file-link]")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Open file|Abrir arquivo/ })).toHaveCount(0);
});
