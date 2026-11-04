// Acceptance M2-E5-T17 on the qa build: the ranking of 200 sites (virtualized, sortable, lazy
// sparklines, smooth scroll), idle sites, the machine monitor and the performance alerts of
// Atenção, with their `forge` commands. Shots land in qa/out/dashboard-projects (gitignored).
import type { QaHandle } from "../src/qa/bootstrap";
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";

const SHOT_DIR = "qa/out/dashboard-projects";

function url(scenario: string, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario, locale: "en-US", vibrancy: "off", ...extra });
  return `/dashboard?${params.toString()}`;
}

const rows = (page: Page) => page.locator("[data-project-row]");

test("ranks 200 sites with a window of rows and sparklines only for those", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  await expect(rows(page).first()).toBeVisible();
  const mounted = await rows(page).count();
  expect(mounted).toBeGreaterThan(3);
  expect(mounted).toBeLessThan(40);
  await expect(page.locator("[data-project-spark] canvas").first()).toBeVisible();
  const series = await page.evaluate(() => {
    const qa = (window as unknown as { __FORGEBOX_QA__: QaHandle }).__FORGEBOX_QA__;
    return qa.bridge.calls.filter((call) => call.method === "stats.series").length;
  });
  expect(series).toBeLessThanOrEqual(mounted);
  await expect(page.getByTestId("dashboard-projects")).toContainText("≈");
  await page.getByTestId("dashboard-projects").scrollIntoViewIfNeeded();
  await page.getByTestId("dashboard-projects").screenshot({ path: `${SHOT_DIR}/ranking.png` });
  await page.getByTestId("dashboard-machine").screenshot({ path: `${SHOT_DIR}/machine.png` });
});

test("sorts by CPU, RAM, p99 and errors", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  const section = page.getByTestId("dashboard-projects");
  await expect(rows(page).first()).toBeVisible();
  await section.getByRole("button", { name: "Sort by p99" }).click();
  await expect(rows(page).first()).toContainText("site1.test");
  await section.getByRole("button", { name: "Sort by Errors" }).click();
  await expect(rows(page).first()).toContainText("site2.test");
  await section.getByRole("button", { name: "Sort by RAM" }).click();
  await expect(section.getByRole("button", { name: "Sort by RAM" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await section.getByRole("button", { name: "Sort by CPU" }).click();
  await expect(section.getByRole("button", { name: "Sort by CPU" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});

test("scrolling the 200-row list keeps frames short", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  await expect(rows(page).first()).toBeVisible();
  const frames = await page.evaluate(async () => {
    const list = document.querySelector<HTMLElement>('[aria-label^="Projects ranked"]');
    if (list === null) throw new Error("list not found");
    const times: number[] = [];
    let last = performance.now();
    const max = list.scrollHeight - list.clientHeight;
    const steps = 120;
    for (let i = 1; i <= steps; i += 1) {
      list.scrollTop = (max * i) / steps;
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          const now = performance.now();
          times.push(now - last);
          last = now;
          resolve();
        });
      });
    }
    return times;
  });
  const sorted = [...frames].sort((a, b) => a - b);
  const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? 0;
  console.log(
    `scroll frames: n=${String(frames.length)} p95=${p95.toFixed(1)} ms max=${Math.max(...frames).toFixed(1)} ms`,
  );
  // Generous on purpose: a shared, throttled CI machine; the budget is "no visible stall".
  expect(p95).toBeLessThan(50);
  expect(await rows(page).count()).toBeLessThan(40);
});

test("idle: the 8 day site is suggested for stopping, the 6 day one is not", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  const idle = page.getByTestId("projects-idle");
  await expect(idle).toBeVisible();
  await expect(idle).toContainText("site3.test");
  await expect(idle).not.toContainText("site4.test");
  await expect(idle.locator("[data-forge-cli]")).toHaveAttribute(
    "data-forge-cli",
    "forge run site3.test web stop",
  );
  await page.screenshot({ path: `${SHOT_DIR}/idle.png` });
});

test("machine monitor and the CLI hints", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  const monitor = page.getByTestId("dashboard-machine");
  await expect(monitor.getByTestId("machine-cpu")).toBeVisible();
  await expect(monitor.getByTestId("machine-ram")).toBeVisible();
  await expect(monitor.getByTestId("machine-disk-root")).toBeVisible();
  const hints = await page
    .locator("[data-forge-cli]")
    .evaluateAll((nodes) => nodes.map((node) => node.getAttribute("data-forge-cli")));
  expect(hints).toContain("forge stats --window 24h");
  expect(hints).toContain("forge top");
});

test("performance alerts: one Atenção item per rule, each with a command", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  const items = page.locator('[data-attention-item^="performance/"]');
  await expect(items).toHaveCount(4);
  // The texts come from the lazy `metrics` namespace: never a raw key.
  await expect(items.first()).toContainText("site1.test is slow");
  await expect(page.getByTestId("dashboard-attention")).not.toContainText("attention.");
  const commands = await items.evaluateAll((nodes) =>
    nodes.map((node) => node.querySelector("[data-forge-cli]")?.getAttribute("data-forge-cli")),
  );
  expect(commands.sort()).toEqual(
    [
      "forge stats site1.test --window 5m",
      "forge stats site2.test --window 5m",
      "forge top",
      "forge top",
    ].sort(),
  );
  await page.screenshot({ path: `${SHOT_DIR}/attention.png` });
});

test("a healthy fleet raises no performance alert", async ({ page }) => {
  await page.goto(url("dashboard"));
  await expect(rows(page).first()).toBeVisible();
  await expect(page.locator('[data-attention-item^="performance/"]')).toHaveCount(0);
});

test("a row opens the Desempenho tab of the site", async ({ page }) => {
  await page.goto(url("dashboard-projects"));
  const first = rows(page).first();
  const id = await first.getAttribute("data-project-row");
  await first.click();
  await expect(page).toHaveURL(new RegExp("/sites/" + (id ?? "") + "[?]tab=performance"));
  await expect(page.getByTestId("page-sites")).toBeVisible();
});

test("pt-BR: ranking, idle and alerts are translated", async ({ page }) => {
  await page.goto(url("dashboard-projects", { locale: "pt-BR" }));
  await expect(page.getByTestId("dashboard-projects")).toContainText("Projetos");
  await expect(page.getByTestId("projects-idle")).toContainText("Ociosos há mais de 7 dias");
  await expect(page.getByTestId("dashboard-machine")).toContainText("Máquina");
  await page.screenshot({ path: `${SHOT_DIR}/pt-BR.png` });
});
