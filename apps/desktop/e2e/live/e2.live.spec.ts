// The E2 screens against the real forgeboxd (M2-E2-T20, docs/07 section 10 and 11): the debug
// Forgebox.exe, a real daemon on a temp portable root, fake tools and a fake php-cgi, driven over
// WebView2's CDP port. scripts/gui-e2e.ps1 builds, creates the root (e2e/live/fixture.mjs),
// starts the app and exports the FORGE_E2E_* variables. One app session, serial scenarios:
//   1 onboarding with a fixture stack        5 a quick setting (MySQL max_connections)
//   2 services start/stop (+ Start < 50 ms)  6 an extension toggle (PHP xsl)
//   3 site add                               7 catalog install with cancel
//   4 live PHP switch between two versions   8 daemon kill and restart, resync without a reload
// plus the parity scan of every [data-action] seen on the way (e2e/live/parity.ts).
// The screens are driven through history.pushState (no reload), so the language picked in
// Settings (English, the labels below) and the in-memory state survive between scenarios.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { join } from "node:path";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { connect, call, ipcState, need } from "./support";
import { forgeAccepts, parityProblems, scanActions, type ScannedAction } from "./parity";

interface InstanceRow {
  tool: string;
  name: string;
  state: string;
  pid: number | null;
}
const services = async (page: Page) =>
  (await call<{ instances: InstanceRow[] }>(page, "services.status")).instances;
const stateOf = async (page: Page, target: string) => {
  const [tool, name] = target.split("/");
  return (await services(page)).find((i) => i.tool === tool && i.name === name)?.state;
};

const root = () => need("FORGE_E2E_ROOT");
const scanned: ScannedAction[] = [];
const scan = async (page: Page, route: string) => {
  scanned.push(...(await scanActions(page, route)));
};

/** Client-side navigation: the router listens to popstate, nothing reloads. */
async function go(page: Page, path: string): Promise<void> {
  await page.evaluate((p) => {
    history.pushState({}, "", p);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, path);
}

async function pickOption(page: Page, trigger: Locator, name: RegExp): Promise<void> {
  await trigger.click();
  await page.getByRole("option", { name }).click();
}

function daemonPid(): number | null {
  try {
    const lock = JSON.parse(
      readFileSync(join(root(), "state", "locks", "forgeboxd.lock"), "utf8"),
    ) as { pid?: unknown };
    return typeof lock.pid === "number" ? lock.pid : null;
  } catch {
    return null;
  }
}

let page: Page;
let close: () => Promise<void>;

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  const connected = await connect();
  page = connected.page;
  close = () => connected.browser.close().catch(() => undefined);
  await expect.poll(async () => (await ipcState(page)).status, { timeout: 60_000 }).toBe("connected");
  // English labels for the whole session (the picker re-renders without a reload).
  await go(page, "/settings");
  await page.locator("select").first().selectOption("en-US");
  await expect(page.getByRole("heading", { level: 1, name: "Settings" })).toBeVisible();
});

test.afterAll(async () => {
  await close();
});

test("1 onboarding with a fixture stack installs nothing and lands on the dashboard", async () => {
  await go(page, "/onboarding");
  const stack = page.locator('label[data-stack-card="fixture-php"]');
  await expect(stack).toBeVisible({ timeout: 20_000 });
  await stack.click();
  await expect(page.getByTestId("plan-total")).toBeVisible();
  await scan(page, "/onboarding (stack)");
  await page.getByRole("button", { name: "Continue" }).click();

  // The suite never touches the certificate store, NRPT or HKCU Run: every system change is off.
  await expect(page.locator('[data-option="dns"]')).toBeVisible();
  for (const id of ["path", "dns", "ca", "autostart"]) {
    const option = page.locator(`[data-option="${id}"]`);
    if ((await option.count()) === 0) continue;
    const toggle = option.getByRole("switch");
    if ((await toggle.getAttribute("aria-checked")) === "true") await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");
  }
  await scan(page, "/onboarding (options)");
  await page.getByRole("button", { name: "Install and set up" }).click();
  await expect(page.locator('[data-run-step="install"]')).toHaveAttribute("data-status", "done", {
    timeout: 60_000,
  });
  for (const id of ["dns", "ca", "path", "autostart"]) {
    await expect(page.locator(`[data-run-step="${id}"]`)).toHaveCount(0);
  }
  await expect(page.getByTestId("first-site")).toBeVisible();
  await scan(page, "/onboarding (run)");
  await page.getByRole("button", { name: "Finish" }).click();
  await expect(page.getByTestId("page-dashboard")).toBeVisible();
  await expect(page.locator("[data-tile]")).toHaveCount(3);
});

test("2 services stop and start from the tile and the Services screen; Start shows Starting in < 50 ms", async () => {
  await go(page, "/dashboard");
  const tile = page.locator('[data-tile][data-target="mysql/default"]');
  await expect(tile).toHaveAttribute("data-state", "running");
  await scan(page, "/dashboard");

  await tile.locator('[data-action="services.stop_instance"] button').click();
  await expect(tile).toHaveAttribute("data-state", "stopped", { timeout: 30_000 });
  expect(await stateOf(page, "mysql/default")).toBe("stopped");

  // Performance mark: the click handler stamps `forge:start-click`, a MutationObserver stamps
  // `forge:start-shown` when the button of this tile shows its loading state.
  await page.evaluate(() => {
    performance.clearMarks();
    document.addEventListener("click", () => performance.mark("forge:start-click"), {
      capture: true,
      once: true,
    });
    const selector =
      '[data-tile][data-target="mysql/default"] [data-action="services.start_instance"] button[data-loading]';
    const observer = new MutationObserver(() => {
      const button = document.querySelector(selector);
      if (button) {
        performance.mark("forge:start-shown", { detail: button.textContent });
        observer.disconnect();
      }
    });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true });
  });
  const start = tile.locator('[data-action="services.start_instance"] button');
  await start.click();
  await page.waitForFunction(() => performance.getEntriesByName("forge:start-shown").length > 0);
  const { ms, label } = await page.evaluate(() => {
    const measure = performance.measure("forge:start-latency", "forge:start-click", "forge:start-shown");
    const shown = performance.getEntriesByName("forge:start-shown")[0] as PerformanceMark;
    return { ms: measure.duration, label: String(shown.detail ?? "") };
  });
  console.log(`PERF start-click -> Starting shown: ${ms.toFixed(1)} ms ("${label.trim()}")`);
  expect(label).toMatch(/Starting/);
  expect(ms).toBeLessThan(50);
  await expect(tile).toHaveAttribute("data-state", "running", { timeout: 30_000 });

  // The Services screen: the same daemon, another service (mailpit).
  await go(page, "/services");
  const row = page.locator('[data-service-row][data-target="mailpit/default"]');
  await expect(row).toHaveAttribute("data-state", "running");
  await scan(page, "/services");
  await row.getByRole("button", { name: "Stop" }).click();
  await expect(row).toHaveAttribute("data-state", "stopped", { timeout: 30_000 });
  expect(await stateOf(page, "mailpit/default")).toBe("stopped");
  await row.getByRole("button", { name: "Start" }).click();
  await expect(row).toHaveAttribute("data-state", "running", { timeout: 30_000 });
  expect(await stateOf(page, "mailpit/default")).toBe("running");
});

test("3 a site is added from a folder and served by the daemon", async () => {
  await go(page, "/sites");
  await expect(page.getByTestId("page-sites")).toBeVisible();
  await page.getByRole("button", { name: "New site" }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Project folder").fill(need("FORGE_E2E_PROJECT"));
  await expect(dialog.getByLabel("Host")).toHaveValue("loja.test");
  // No certificate: HTTPS stays off (the CA is not installed in this suite).
  const https = dialog.getByRole("switch", { name: "HTTPS" });
  if ((await https.getAttribute("aria-checked")) === "true") await https.click();
  await scan(page, "/sites (new site)");
  await dialog.getByRole("button", { name: "Create site" }).click();
  await expect(page.locator('[data-site-card]').filter({ hasText: "loja.test" })).toHaveCount(1, {
    timeout: 30_000,
  });
  const list = await call<{ sites: { host: string; php: { series: string | null } }[] }>(
    page,
    "sites.list",
  );
  expect(list.sites.map((s) => s.host)).toEqual(["loja.test"]);
  await scan(page, "/sites");
});

test("4 the PHP of the site switches live between two versions", async () => {
  await go(page, "/sites");
  await page.getByRole("link", { name: /Details of loja\.test|loja\.test/ }).first().click();
  await expect(page.getByTestId("site-general")).toBeVisible();
  await scan(page, "/sites/<id> General");
  for (const tab of ["Processes", "Environment", "Diagnostics"]) {
    await page.getByRole("tab", { name: tab }).click();
    await scan(page, `/sites/<id> ${tab}`);
  }
  await page.getByRole("tab", { name: "General" }).click();
  const combo = page.getByRole("combobox", { name: "PHP version" });
  const current = (await combo.innerText()).trim();
  const other = current.startsWith("8.3") ? "8.4" : "8.3";

  const upstream = (host: string): string => {
    const site = readFileSync(join(root(), "etc", "nginx", "sites", `${host}.conf`), "utf8");
    const name = /fastcgi_pass\s+([^;\s]+);/.exec(site)?.[1] ?? "";
    return name;
  };
  const before = upstream("loja.test");
  await pickOption(page, combo, new RegExp(`^${other.replace(".", "\\.")}`));
  await expect(page.getByText(`PHP ${other} is active`)).toBeVisible({ timeout: 60_000 });
  const switched = upstream("loja.test");
  expect(switched).toBe(`php_${other.replace(".", "_")}`);
  expect(switched).not.toBe(before);
  const pools = await call<{ pools: { series: string; state: string }[] }>(page, "php.pool_status", {});
  expect(pools.pools.find((p) => p.series === other)?.state).toBe("running");

  // And back: the first version serves the site again.
  await pickOption(page, combo, new RegExp(`^${current.slice(0, 3).replace(".", "\\.")}`));
  await expect(page.getByText(`PHP ${current.slice(0, 3)} is active`)).toBeVisible({
    timeout: 60_000,
  });
  expect(upstream("loja.test")).toBe(before);
});

test("5 a quick setting is applied to MySQL and written to its config", async () => {
  await go(page, "/services");
  await page.locator('[data-service-row][data-target="mysql/default"] button').first().click();
  await expect(page.getByTestId("service-sheet")).toBeVisible();
  await page.getByRole("tab", { name: "Configuration" }).click();
  const form = page.getByTestId("quick-settings");
  await expect(form).toBeVisible();
  await form.getByRole("textbox", { name: "Max connections" }).fill("321");
  const save = form.locator('[data-action="services.set_settings"]');
  await expect(save).toHaveAttribute(
    "data-forge-cli",
    "forge service set mysql/default max_connections=321",
  );
  await scan(page, "/services (Configuration)");
  await save.getByRole("button", { name: "Apply changes" }).click();
  // `done` only lasts a moment (ActionButton DONE_MS): wait for the end of `running` instead,
  // then read the daemon's side of it (`failed` would stay on screen).
  await expect(save).not.toHaveAttribute("data-phase", "running", { timeout: 60_000 });
  await expect(save).not.toHaveAttribute("data-phase", "failed");
  const ini = readFileSync(join(root(), "etc", "mysql", "default", "my.ini"), "utf8");
  expect(ini).toMatch(/max_connections\s*=\s*321/);
  await expect.poll(() => stateOf(page, "mysql/default"), { timeout: 30_000 }).toBe("running");
  for (const tab of ["Overview", "Logs", "Instances"]) {
    await page.getByRole("tab", { name: tab }).click();
    await scan(page, `/services (${tab})`);
  }
  await page.getByRole("tab", { name: "Versions" }).click();
  await scan(page, "/services (Versions)");
  await page.getByRole("tab", { name: "Advanced" }).click();
  await scan(page, "/services (Advanced)");
  await page.keyboard.press("Escape");
});

test("6 a PHP extension is toggled on and off and the overlay follows", async () => {
  await go(page, "/php");
  const column = page.locator('[data-php-series="8.4"]');
  await expect(column).toBeVisible();
  await scan(page, "/php");
  const ext = column.getByTestId("extensions");
  await ext.getByRole("searchbox").fill("xsl");
  const toggle = ext.getByRole("switch", { name: "xsl extension" });
  const overlay = join(root(), "etc", "php", "8.4", "conf.d", "10-forgebox.ini");
  const has = () => readFileSync(overlay, "utf8").includes("\nextension = xsl\n");
  await expect(toggle).not.toBeChecked();
  expect(has()).toBe(false);
  await toggle.click();
  await expect(toggle).toBeChecked({ timeout: 30_000 });
  await expect.poll(has, { timeout: 30_000 }).toBe(true);
  await toggle.click();
  await expect(toggle).not.toBeChecked({ timeout: 30_000 });
  await expect.poll(has, { timeout: 30_000 }).toBe(false);
  const list = await call<{ extensions: { name: string; enabled: boolean }[] }>(page, "php.ext_list", {
    version: "8.4",
  });
  expect(list.extensions.find((e) => e.name === "xsl")?.enabled).toBe(false);
});

test("7 a catalog install is cancelled and leaves nothing behind", async () => {
  // The release URL of the fixture tool points at a socket that never answers TLS.
  const sockets = new Set<import("node:net").Socket>();
  const stall: Server = createServer((socket) => {
    sockets.add(socket);
    socket.on("error", () => undefined);
    socket.on("close", () => sockets.delete(socket));
  });
  await new Promise<void>((ok) => stall.listen(Number(need("FORGE_E2E_STALL_PORT")), "127.0.0.1", ok));
  try {
    await go(page, "/catalog");
    await expect(page.getByTestId("page-catalog")).toBeVisible();
    await scan(page, "/catalog");
    await go(page, "/catalog/fixture-slow");
    const install = page.getByRole("button", { name: "Install", exact: true });
    await expect(install).toBeEnabled({ timeout: 30_000 });
    await scan(page, "/catalog/<tool>");
    await install.click();
    const job = page.locator("[data-install-job]");
    await expect(job).toBeVisible({ timeout: 30_000 });
    await expect.poll(() => sockets.size, { timeout: 30_000 }).toBeGreaterThan(0);
    await page.getByRole("button", { name: "Cancel" }).click();
    await expect(page.locator('[data-install-job][data-state="cancelled"]')).toBeVisible({
      timeout: 30_000,
    });
    const listed = await call<{ tools: { id: string; installed: unknown[] }[] }>(page, "tools.list", {});
    expect(listed.tools.find((t) => t.id === "fixture-slow")?.installed ?? []).toEqual([]);
    expect(existsSync(join(root(), "bin", "fixture-slow"))).toBe(false);
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise<void>((ok) => {
      stall.close(() => {
        ok();
      });
    });
  }
});

test("8 killing the daemon shows disconnected within 2 s and the screens resync without a reload", async () => {
  await go(page, "/dashboard");
  // Three instances plus the PHP pool that the switch of scenario 4 started.
  await expect(page.locator("[data-tile]")).toHaveCount(4);
  // One marker that only survives while the document is the same one.
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__liveMarker = "same-document";
    const w = window as unknown as { __conn: { t: number; status: string }[] };
    w.__conn = [];
    const internals = (window as unknown as {
      __TAURI_INTERNALS__: {
        transformCallback: (cb: (m: unknown) => void) => number;
        invoke: (c: string, a: unknown) => Promise<unknown>;
      };
    }).__TAURI_INTERNALS__;
    const handler = internals.transformCallback((message) => {
      const status = (message as { payload?: { status?: string } }).payload?.status;
      if (status) w.__conn.push({ t: Date.now(), status });
    });
    void internals.invoke("plugin:event|listen", {
      event: "forge://connection",
      target: { kind: "Any" },
      handler,
    });
  });
  const before = daemonPid();
  expect(before).not.toBeNull();
  const killedAt = Date.now();
  execFileSync("taskkill", ["/PID", String(before), "/T", "/F"], { stdio: "ignore" });

  const log = () =>
    page.evaluate(() => (window as unknown as { __conn: { t: number; status: string }[] }).__conn);
  await expect
    .poll(async () => (await log()).some((e) => e.status !== "connected"), { timeout: 2_000 })
    .toBe(true);
  const first = (await log()).find((e) => e.status !== "connected");
  const disconnectedAfter = (first?.t ?? Infinity) - killedAt;
  console.log(`PERF kill -> ${first?.status ?? "?"} after ${String(disconnectedAfter)} ms`);
  expect(disconnectedAfter).toBeLessThan(2_000);

  await expect.poll(async () => (await ipcState(page)).status, { timeout: 30_000 }).toBe("connected");
  expect(daemonPid()).not.toBe(before);
  expect(await page.evaluate(() => (window as unknown as Record<string, unknown>).__liveMarker)).toBe(
    "same-document",
  );

  // Resynced: the tiles show the new daemon's services, and live events flow again.
  const tile = page.locator('[data-tile][data-target="mailpit/default"]');
  await expect(tile).toHaveAttribute("data-state", "running", { timeout: 30_000 });
  await call(page, "services.stop", { targets: ["mailpit/default"] });
  await expect(tile).toHaveAttribute("data-state", "stopped", { timeout: 30_000 });
  await call(page, "services.start", { targets: ["mailpit/default"] });
  await expect(tile).toHaveAttribute("data-state", "running", { timeout: 30_000 });
  expect(await page.evaluate(() => (window as unknown as Record<string, unknown>).__liveMarker)).toBe(
    "same-document",
  );
});

test("parity: every [data-action] of the E2 routes has a validated forge command", () => {
  const routes = new Set(scanned.map((s) => s.route));
  console.log(`PARITY ${String(scanned.length)} actions on ${String(routes.size)} views`);
  expect(scanned.length).toBeGreaterThan(20);
  const problems = parityProblems(scanned, forgeAccepts(need("FORGE_E2E_FORGE")));
  expect(problems).toEqual([]);
});
