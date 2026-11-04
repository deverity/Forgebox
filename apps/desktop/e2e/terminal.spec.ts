// /terminal in a real browser against the fake bridge (M2-E3-T15): echo latency while a 20 MB
// stream is being written, long tasks, shortcuts, bundled fonts offline and the exit banner.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const URL = "/terminal?scenario=populated";
const FLOOD_BYTES = 20 * 1024 * 1024;
const PROMPT = "PS C:\\Forgebox>";

interface Probe {
  produced: number;
  latencies: number[];
  longTasks: number[];
  sent: number;
}

declare global {
  interface Window {
    __probe?: Probe;
  }
}

/** Opens the first tab and waits for the prompt the fake shell prints. */
async function openFirstTab(page: Page) {
  await page.goto(URL);
  await expect(page.getByTestId("page-terminal")).toBeVisible();
  await page.getByRole("button", { name: /new terminal|novo terminal/i }).click();
  await expect
    .poll(() => page.evaluate(() => window.__FORGEBOX_TERM__?.text() ?? ""), { timeout: 15_000 })
    .toContain(PROMPT);
}

function percentile(values: number[], p: number): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(p * sorted.length) - 1)] ?? 0;
}

// A timing budget on a shared runner: the p95 of 44 echoes moved between 25 and 60 ms across
// runs on the same quiet machine (30 Hz RDP vsync, M2-E1-T21), so it gets two retries like the
// three runs of qa:perf; a real regression fails all three.
test.describe("terminal flood budget", () => {
  test.describe.configure({ retries: 2 });

  test("a 20 MB stream keeps echo under 50 ms (p95) and has no long task over 100 ms", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await openFirstTab(page);

    // Installs the probes and starts the flood; returns at once so typing runs meanwhile.
    await page.evaluate((total) => {
      interface Term {
        onTitleChange(cb: (title: string) => void): void;
        focus(): void;
      }
      const hook = window.__FORGEBOX_TERM__;
      const qa = window.__FORGEBOX_QA__;
      if (!hook || !qa) throw new Error("QA hooks missing");
      const term = hook.term() as Term;
      const id = hook.id();
      if (id === null) throw new Error("terminal not opened");
      const probe: Probe = { produced: 0, latencies: [], longTasks: [], sent: 0 };
      window.__probe = probe;

      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) probe.longTasks.push(entry.duration);
      }).observe({ type: "longtask" });

      // The fake shell follows each echo with a title marker `E:<characters echoed so far>`: when the
      // emulator parses it, every typed character up to that count has been handled.
      const sent: number[] = [];
      document.addEventListener(
        "keydown",
        (event) => {
          if (event.key.length === 1) sent.push(performance.now());
          probe.sent = sent.length;
        },
        true,
      );
      term.onTitleChange((title) => {
        const count = Number(title.replace("E:", ""));
        if (Number.isNaN(count)) return;
        const now = performance.now();
        while (probe.latencies.length < count && probe.latencies.length < sent.length) {
          probe.latencies.push(now - (sent[probe.latencies.length] ?? 0));
        }
      });
      term.focus();

      // 16 KiB of colored whole lines every ~4 ms until 20 MiB went out.
      const line = (n: number) =>
        `\u001b[32m${String(n).padStart(8, "0")}\u001b[0m \u001b[1mGET\u001b[0m /assets/app-${String(n % 977)}.js 200 ${"x".repeat(40)}\r\n`;
      const encoder = new TextEncoder();
      let n = 0;
      let first = true;
      const timer = setInterval(() => {
        let chunk = first ? "\r\n" : "";
        first = false;
        while (chunk.length < 16 * 1024) {
          chunk += line(n);
          n += 1;
        }
        probe.produced += encoder.encode(chunk).length;
        qa.bridge.emitTerminal(id, { kind: "output", data: chunk });
        if (probe.produced >= total) clearInterval(timer);
      }, 4);
    }, FLOOD_BYTES);

    // The first flood chunk moves the cursor off the prompt line before the first key.
    await page.waitForTimeout(300);
    // Type while the stream runs (real key events through the emulator's textarea).
    const keys = "forgeboxterminalechoprobeunderafloodedstream".split("");
    for (const key of keys) {
      await page.keyboard.press(key);
      await page.waitForTimeout(120);
    }
    await expect
      .poll(() => page.evaluate(() => window.__probe?.produced ?? 0), { timeout: 90_000 })
      .toBeGreaterThanOrEqual(FLOOD_BYTES);
    // Let the emulator drain, then read the probes.
    await page.waitForTimeout(1500);
    const probe = await page.evaluate(() => window.__probe);
    if (!probe) throw new Error("probe missing");

    expect(probe.latencies.length).toBeGreaterThanOrEqual(keys.length - 2);
    const p95 = percentile(probe.latencies, 0.95);
    const worstTask = Math.max(0, ...probe.longTasks);
    console.log(
      `terminal flood: ${String(probe.latencies.length)} echoes, p50 ${percentile(probe.latencies, 0.5).toFixed(1)} ms, p95 ${p95.toFixed(1)} ms, max ${Math.max(...probe.latencies).toFixed(1)} ms; long tasks ${String(probe.longTasks.length)} (worst ${worstTask.toFixed(0)} ms); renderer ${await page.evaluate(() => window.__FORGEBOX_TERM__?.renderer() ?? "?")}`,
    );
    expect(p95).toBeLessThan(50);
    expect(worstTask).toBeLessThanOrEqual(100);
  });
});

test("Ctrl+` opens a terminal tab from any screen and the tab connects", async ({ page }) => {
  await page.goto("/dashboard?scenario=populated");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(async () => {
    await page.keyboard.press("Control+Backquote");
    await expect(page.getByTestId("page-terminal")).toBeVisible({ timeout: 1000 });
  }).toPass({ timeout: 10_000 });
  await expect(page.getByRole("tab")).toHaveCount(1);
  await expect
    .poll(() => page.evaluate(() => window.__FORGEBOX_TERM__?.text() ?? ""), { timeout: 15_000 })
    .toContain(PROMPT);
});

test("bundled fonts load without the network", async ({ page }) => {
  // Anything that is not the local preview server fails: the fonts must come from the bundle.
  await page.route(/^https?:\/\/(?!127\.0\.0\.1|localhost)/, (route) => route.abort());
  await openFirstTab(page);
  const loaded = await page.evaluate(async () => {
    const faces = await document.fonts.load('13px "JetBrains Mono"');
    return { faces: faces.length, ok: document.fonts.check('13px "JetBrains Mono"') };
  });
  expect(loaded.faces).toBeGreaterThan(0);
  expect(loaded.ok).toBe(true);
});

test("exit shows the banner with the code and Reopen starts a fresh session", async ({ page }) => {
  await openFirstTab(page);
  await page
    .getByTestId(/terminal-pane-/)
    .first()
    .click();
  await page.keyboard.type("exit");
  await page.keyboard.press("Enter");
  const banner = page.getByTestId("terminal-exited");
  await expect(banner).toContainText(/\(c(ó|o)digo 3\)|\(code 3\)/);
  await banner.getByRole("button", { name: /reabrir|reopen/i }).click();
  await expect(banner).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => window.__FORGEBOX_TERM__?.text() ?? ""), { timeout: 15_000 })
    .toContain(PROMPT);
});
