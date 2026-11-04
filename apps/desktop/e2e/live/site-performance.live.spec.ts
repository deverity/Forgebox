// The site "Desempenho" tab against the real forgeboxd (M2-E5-T16, docs/14 section 4.1): a site
// is added, the global metrics switch is turned on, and synthetic access lines in nginx's
// `log_format forgebox` (the fixture of crates/forge-daemon/tests/metrics.rs) are appended to the
// log the daemon follows. The tab then shows what the daemon computed: KPIs, charts with the live
// 60 s stream, routes and errors; the pool toggle runs the real `sites.pool_mode` job.
// Run alone: scripts/gui-e2e.ps1 -Grep "site-performance". Shots: qa/out/site-performance/live-*.png.
import { appendFileSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { call, connect, ipcState, need } from "./support";

const HOST = "perf.test";
const SHOT_DIR = "qa/out/site-performance";

interface SiteRow {
  id: string;
  host: string;
  pool_mode?: string;
  metrics_enabled?: boolean;
}
interface InstanceRow {
  tool: string;
  state: string;
}

async function go(page: Page, path: string): Promise<void> {
  await page.evaluate((p) => {
    history.pushState({}, "", p);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, path);
}

/** One line of nginx's `log_format forgebox` that ends now. */
function line(status: number, rtMs: number, method: string, uri: string): string {
  const now = Date.now();
  const sec = Math.floor(now / 1000);
  const ms = String(now % 1000).padStart(3, "0");
  const rt = `${String(Math.floor(rtMs / 1000))}.${String(Math.floor(rtMs % 1000)).padStart(3, "0")}`;
  const ut = `${String(Math.floor(rtMs / 2000))}.${String(Math.floor((rtMs / 2) % 1000)).padStart(3, "0")}`;
  return `{"t":"${String(sec)}.${ms}","host":"${HOST}","s":${String(status)},"rt":${rt},"ut":"${ut}","b":2048,"m":"${method}","u":"${uri}","up":"127.0.0.1:19000"}\n`;
}

const TRAFFIC: [number, number, string, string][] = [
  [200, 22, "GET", "/"],
  [200, 31, "GET", "/products/12"],
  [200, 44, "GET", "/products/97"],
  [200, 18, "GET", "/assets/app.css"],
  [200, 95, "GET", "/search"],
  [200, 160, "POST", "/login"],
  [200, 420, "POST", "/checkout/7"],
  [404, 6, "GET", "/missing"],
  [500, 380, "POST", "/checkout/9"],
  [502, 1210, "GET", "/api/orders"],
];

let page: Page;
let close: () => Promise<void>;
let feeder: ReturnType<typeof setInterval> | null = null;
let site: SiteRow;

// Opt-in: the whole E2 live run does not need to pay for a 2-minute traffic scenario.
test.skip(
  process.env.FORGE_E2E_PERF !== "1",
  "set FORGE_E2E_PERF=1 to run the site-performance live scenario",
);

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  const connected = await connect();
  page = connected.page;
  close = () => connected.browser.close().catch(() => undefined);
  await expect
    .poll(async () => (await ipcState(page)).status, { timeout: 60_000 })
    .toBe("connected");
  await go(page, "/settings");
  await page.locator("select").first().selectOption("en-US");
  await expect(page.getByRole("heading", { level: 1, name: "Settings" })).toBeVisible();
});

test.afterAll(async () => {
  if (feeder !== null) clearInterval(feeder);
  await close();
});

test("site-performance 1 a site, metrics on and synthetic traffic reach the daemon", async () => {
  const project = mkdtempSync(join(tmpdir(), "forgebox-perf-"));
  mkdirSync(project, { recursive: true });
  writeFileSync(join(project, "index.php"), "<?php echo 'ok';\n");
  await call(page, "sites.add", { dir: project.replaceAll("\\", "/"), host: HOST, secure: false });
  await expect
    .poll(
      async () =>
        (await call<{ sites: SiteRow[] }>(page, "sites.list")).sites.find((s) => s.host === HOST),
      { timeout: 60_000 },
    )
    .toBeTruthy();
  site = (await call<{ sites: SiteRow[] }>(page, "sites.list")).sites.find(
    (s) => s.host === HOST,
  ) as SiteRow;
  await call(page, "system.config_set", { key: "metrics.enabled", value: true });
  // The web server must be up for the daemon to follow its access log.
  const nginx = async () =>
    (await call<{ instances: InstanceRow[] }>(page, "services.status")).instances.find(
      (i) => i.tool === "nginx",
    )?.state;
  if ((await nginx()) !== "running") await call(page, "services.start", { targets: ["nginx"] });
  await expect.poll(nginx, { timeout: 60_000 }).toBe("running");

  const log = join(need("FORGE_E2E_ROOT"), "logs", "nginx", "access.json");
  mkdirSync(join(need("FORGE_E2E_ROOT"), "logs", "nginx"), { recursive: true });
  let n = 0;
  feeder = setInterval(() => {
    // ~16 requests per second, the same mix every time (no randomness: stable screenshots).
    let text = "";
    for (let i = 0; i < 8; i += 1) {
      const row = TRAFFIC[(n + i * 3) % TRAFFIC.length] as [number, number, string, string];
      text += line(row[0], row[1] + ((n + i) % 7) * 3, row[2], row[3]);
    }
    n += 1;
    appendFileSync(log, text);
  }, 500);
  await expect
    .poll(
      async () =>
        (
          await call<{ sites: { requests: number }[] }>(page, "stats.get", {
            site: HOST,
            window: "1m",
          })
        ).sites[0]?.requests ?? 0,
      { timeout: 60_000, intervals: [1000] },
    )
    .toBeGreaterThan(30);
});

test("site-performance 2 the tab shows the daemon's numbers, light and dark", async () => {
  test.setTimeout(150_000);
  await go(page, `/sites/${site.id}`);
  await page.getByRole("tab", { name: "Performance" }).click();
  const kpis = page.getByTestId("perf-kpis");
  await expect(kpis).toBeVisible();
  await expect(kpis.locator("[data-kpi='rps']")).not.toContainText("—", { timeout: 30_000 });
  await expect(kpis.locator("[data-kpi='p99']")).not.toContainText("—");
  // Let the 60 s window fill with live samples.
  await expect(page.locator("[data-chart-state='ready']").first()).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(40_000);
  await expect(page.getByTestId("perf-badges")).toContainText("Live");
  await expect(page.getByTestId("perf-routes").getByRole("row").nth(1)).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.getByTestId("perf-errors")).toContainText("50");
  await page.setViewportSize({ width: 1280, height: 1500 });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(800);
    await page.screenshot({ path: `${SHOT_DIR}/live-${scheme}.png`, fullPage: true });
  }
  await page.getByRole("radio", { name: "1 h" }).click();
  await expect(page.getByTestId("site-performance")).toHaveAttribute("data-range", "1h");
  await expect(page.locator("[data-chart-state='ready']").first()).toBeVisible({ timeout: 15_000 });
  await page.screenshot({ path: `${SHOT_DIR}/live-1h-dark.png`, fullPage: true });
});

test("site-performance 3 the dedicated pool toggle runs the real job", async () => {
  test.setTimeout(120_000);
  const pool = page.getByTestId("perf-pool-toggle");
  await expect(pool).toHaveAttribute("data-forge-cli", `forge site pool ${HOST} dedicated`);
  await pool.getByRole("switch").click();
  await expect(pool).toHaveAttribute("data-phase", /running|done/);
  await expect(pool).toHaveAttribute("data-phase", "done", { timeout: 90_000 });
  await expect(pool.getByRole("switch")).toBeChecked();
  const row = (await call<{ sites: SiteRow[] }>(page, "sites.list")).sites.find(
    (s) => s.host === HOST,
  );
  expect(row?.pool_mode).toBe("dedicated");
  await page.screenshot({ path: `${SHOT_DIR}/live-pool-dedicated.png`, fullPage: true });
});
