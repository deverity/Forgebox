// The site "Desempenho" tab on the qa build against the fake bridge (M2-E5-T16): data, the
// states that replace the panels, the range control, the pool toggle (loading -> success and
// loading -> error with its CLI) and no layout shift between skeleton and data. Shots land in
// qa/out/site-performance (light and dark).
import type { Page } from "@playwright/test";
import { axeViolations, expect, test, trackCls } from "./fixtures";

const SHOT_DIR = "qa/out/site-performance";
const PHP_SITE = "01QAS1TE000000000000000001";
const PROXY_SITE = "01QAS1TE000000000000000003";

function url(site: string, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ scenario: "populated", locale: "en-US", ...extra });
  return `/sites/${site}?${params.toString()}`;
}

async function openTab(page: Page, name = "Performance") {
  const trigger = page.getByRole("tab", { name });
  await trigger.click();
  await expect(trigger).toHaveAttribute("data-state", "active");
}

async function loaded(page: Page) {
  await expect(page.locator("[data-chart-state='ready']").first()).toBeVisible();
  await expect(page.locator("[data-skeleton]")).toHaveCount(0);
}

for (const theme of ["light", "dark"] as const) {
  test(`shows KPIs, charts, tables and controls (${theme})`, async ({ page }) => {
    // Tall enough for the whole tab: the shot is evidence of every region.
    await page.setViewportSize({ width: 1280, height: 1500 });
    await page.goto(url(PHP_SITE, { theme }));
    await openTab(page);
    await loaded(page);
    const kpis = page.getByTestId("perf-kpis");
    await expect(kpis).toContainText("16 req/s");
    await expect(kpis).toContainText("142 ms");
    await expect(kpis).toContainText("≈");
    await expect(page.getByTestId("perf-routes").getByRole("row")).toHaveCount(9);
    await expect(page.getByTestId("perf-errors")).toContainText("502");
    // Three charts (latency, throughput, resource) with their markers.
    await expect(page.locator("[data-chart]")).toHaveCount(3);
    await expect(page.locator("[data-marker-kind]").first()).toBeVisible();
    await expect(page.getByText(/deep instrumentation/i)).toHaveCount(0);
    expect(await axeViolations(page)).toEqual([]);
    await page.screenshot({ path: `${SHOT_DIR}/performance-${theme}.png`, fullPage: true });
  });
}

test("renders the pt-BR copy", async ({ page }) => {
  await page.goto(url(PHP_SITE, { locale: "pt-BR" }));
  await openTab(page, "Desempenho");
  await loaded(page);
  await expect(page.getByText("Rotas mais lentas")).toBeVisible();
  await expect(page.getByText("Erros recentes")).toBeVisible();
  await expect(page.getByText("Pool dedicado")).toBeVisible();
  await expect(page.getByText("Coletar métricas deste site")).toBeVisible();
  await expect(page.getByRole("radio", { name: "7 dias" })).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/performance-pt-br.png`, fullPage: true });
});

test("the range control redraws the charts for 1 h, 24 h and 7 d", async ({ page }) => {
  await page.goto(url(PHP_SITE));
  await openTab(page);
  await loaded(page);
  const tab = page.getByTestId("site-performance");
  await expect(tab).toHaveAttribute("data-range", "1m");
  for (const [label, range] of [
    ["1 h", "1h"],
    ["24 h", "24h"],
    ["7 days", "7d"],
    ["60 s", "1m"],
  ] as const) {
    await page.getByRole("radio", { name: label }).click();
    await expect(tab).toHaveAttribute("data-range", range);
    await loaded(page);
  }
});

test("the pool toggle goes loading -> success with its command", async ({ page }) => {
  await page.goto(url(PHP_SITE, { metrics: "slow-pool" }));
  await openTab(page);
  await loaded(page);
  const pool = page.getByTestId("perf-pool-toggle");
  await expect(pool).toHaveAttribute("data-forge-cli", "forge site pool loja.test dedicated");
  await pool.getByRole("switch").click();
  await expect(pool).toHaveAttribute("data-phase", "running");
  await expect(pool.getByText("Applying…")).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/pool-applying.png`, fullPage: true });
  await expect(pool).toHaveAttribute("data-phase", "done");
  await expect(pool.getByRole("switch")).toBeChecked();
  await expect(pool).toHaveAttribute("data-forge-cli", "forge site pool loja.test shared");
});

test("the pool toggle goes loading -> error and keeps the command", async ({ page }) => {
  await page.goto(url(PHP_SITE, { metrics: "pool-fails" }));
  await openTab(page);
  await loaded(page);
  const pool = page.getByTestId("perf-pool-toggle");
  await pool.getByRole("switch").click();
  await expect(pool).toHaveAttribute("data-phase", "running");
  await expect(pool).toHaveAttribute("data-phase", "failed");
  await expect(pool.getByRole("switch")).not.toBeChecked();
  await expect(pool.locator("[data-error-notice]")).toBeVisible();
  await expect(pool).toHaveAttribute("data-forge-cli", "forge site pool loja.test dedicated");
  await page.screenshot({ path: `${SHOT_DIR}/pool-failed.png`, fullPage: true });
});

test("a proxy site has no resource chart and explains the disabled pool toggle", async ({
  page,
}) => {
  await page.goto(url(PROXY_SITE));
  await openTab(page);
  await expect(page.getByTestId("perf-no-resource")).toBeVisible();
  const pool = page.getByTestId("perf-pool-toggle");
  await expect(pool.getByRole("switch")).toBeDisabled();
  await expect(pool.getByTestId("perf-pool-toggle-reason")).toContainText("Only PHP sites");
  await page.screenshot({ path: `${SHOT_DIR}/proxy.png`, fullPage: true });
});

test("metrics off globally: empty state with the action and the forge config command", async ({
  page,
}) => {
  await page.goto(url(PHP_SITE, { metrics: "global-off" }));
  await openTab(page);
  const state = page.getByTestId("perf-state-global-off");
  await expect(state).toBeVisible();
  await expect(
    state.locator("[data-forge-cli='forge config set metrics.enabled true']"),
  ).toHaveCount(2);
  await page.screenshot({ path: `${SHOT_DIR}/global-off.png`, fullPage: true });
  await state.getByRole("button", { name: "Turn metrics on" }).click();
  await expect(page.getByTestId("perf-kpis")).toBeVisible();
});

test("metrics off for the site: turning it on brings the panels back", async ({ page }) => {
  await page.goto(url(PHP_SITE, { metrics: "site-off" }));
  await openTab(page);
  const state = page.getByTestId("perf-state-site-off");
  await expect(state).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/site-off.png`, fullPage: true });
  await state.getByRole("button", { name: "Collect metrics for this site" }).click();
  await expect(page.getByTestId("perf-kpis")).toBeVisible();
});

test("no traffic: says so and keeps the empty charts", async ({ page }) => {
  await page.goto(url(PHP_SITE, { metrics: "no-traffic" }));
  await openTab(page);
  await expect(page.getByTestId("perf-no-traffic")).toBeVisible();
  await expect(page.locator("[data-chart-state='empty']").first()).toBeVisible();
  await page.screenshot({ path: `${SHOT_DIR}/no-traffic.png`, fullPage: true });
});

test("remote context shows the badge", async ({ page }) => {
  await page.goto(url(PHP_SITE, { scenario: "remote" }));
  await openTab(page);
  await expect(page.getByTestId("perf-remote")).toBeVisible();
});

test("no layout shift between skeleton and data", async ({ page }) => {
  const cls = await trackCls(page);
  await page.goto(url(PHP_SITE, { latency: "400" }));
  await page.getByRole("tab", { name: "Performance" }).click();
  const tab = page.getByTestId("site-performance");
  await expect(tab).toBeVisible();
  // Skeleton first: the boxes of every region, then again once the data is in.
  const regions = ["perf-kpis", "perf-charts", "perf-routes", "perf-errors"] as const;
  const measure = async () =>
    Promise.all(
      regions.map(async (id) => {
        const box = await page.getByTestId(id).boundingBox();
        return box === null ? null : { y: box.y, height: box.height };
      }),
    );
  await expect(page.locator("[data-skeleton]").first()).toBeVisible();
  const before = await measure();
  await loaded(page);
  await expect(page.getByTestId("perf-routes").getByRole("row")).toHaveCount(9);
  const after = await measure();
  expect(after).toEqual(before);
  expect(await cls()).toBeLessThan(0.01);
});

test.describe("pt-BR copy of the states", () => {
  const pt = { locale: "pt-BR" };

  test("metrics off globally", async ({ page }) => {
    await page.goto(url(PHP_SITE, { ...pt, metrics: "global-off" }));
    await openTab(page, "Desempenho");
    const state = page.getByTestId("perf-state-global-off");
    await expect(state.getByText("As métricas estão desligadas")).toBeVisible();
    await expect(state.getByRole("button", { name: "Ligar métricas" })).toBeVisible();
  });

  test("metrics off for the site", async ({ page }) => {
    await page.goto(url(PHP_SITE, { ...pt, metrics: "site-off" }));
    await openTab(page, "Desempenho");
    const state = page.getByTestId("perf-state-site-off");
    await expect(state.getByText("Métricas desligadas para este site")).toBeVisible();
  });

  test("no traffic", async ({ page }) => {
    await page.goto(url(PHP_SITE, { ...pt, metrics: "no-traffic" }));
    await openTab(page, "Desempenho");
    await expect(page.getByTestId("perf-no-traffic")).toContainText("Sem tráfego neste intervalo");
  });

  test("proxy site", async ({ page }) => {
    await page.goto(url(PROXY_SITE, pt));
    await openTab(page, "Desempenho");
    await expect(page.getByTestId("perf-pool-toggle-reason")).toContainText(
      "Só sites PHP têm pool",
    );
  });

  test("remote daemon", async ({ page }) => {
    await page.goto(url(PHP_SITE, { ...pt, scenario: "remote" }));
    await openTab(page, "Desempenho");
    await expect(page.getByTestId("perf-remote")).toContainText("Daemon remoto");
  });
});
