// The E3 screens against a real daemon (M2-E3-T21, docs/07 section 10 and 11): the debug
// Forgebox.exe plus examples/live_daemon (forgeboxd with a MemoryTrustStore and a fake UAC) on a
// temp portable root, with the fake Mailpit, the fake database client (fake_db_client), a fake
// nginx and a fixture access log, driven over WebView2's CDP port. scripts/gui-e2e.ps1 -Suite e3
// builds, creates the root (e2e/live/fixture.mjs), starts the app and exports FORGE_E2E_*.
// One app session, serial scenarios:
//   1 mail: list, read, delete (with the undo window) and the mail.changed badge
//   2 terminal: input reaches a real shell, `exit` shows the ended banner
//   3 logs: the site target reads the fixture access log (access columns, host filter)
//   4 doctor: the CA is untrusted, Fix installs it (memory store) and the row changes in place
//   5 settings: a config_set from another client shows up in the open screen
//   6 databases: create, export with cancel, export, import
// plus the parity scan of every [data-action] seen on the way (e2e/live/parity.ts).
// Nothing touches HKCU, the certificate stores or NRPT. Screens are driven through
// history.pushState (no reload), like the E2 suite.
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { call, connect, ipcState, need } from "../support";
import { forgeAccepts, parityProblems, scanActions, type ScannedAction } from "../parity";

const root = () => need("FORGE_E2E_ROOT");
const mysqlBin = () => join(root(), "bin", "mysql", "8.4.13", "bin");
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

interface Job {
  id: string;
  state: string;
  error?: unknown;
}
async function finish(page: Page, job: Job): Promise<Job> {
  let now = job;
  for (let i = 0; i < 300 && (now.state === "running" || now.state === "queued"); i += 1) {
    await page.waitForTimeout(200);
    now = await call<Job>(page, "jobs.get", { job_id: job.id });
  }
  return now;
}

interface InstanceRow {
  tool: string;
  name: string;
  state: string;
  ports: { name: string; port: number }[];
}
const instance = async (page: Page, tool: string) =>
  (await call<{ instances: InstanceRow[] }>(page, "services.status")).instances.find(
    (i) => i.tool === tool,
  );

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
  // The discovered tools autostart; wait for the two services the scenarios need.
  for (const tool of ["mysql", "mailpit"]) {
    await expect
      .poll(async () => (await instance(page, tool))?.state, { timeout: 60_000 })
      .toBe("running");
  }
});

test.afterAll(async () => {
  await close();
});

test("1 mail: list, read, delete with undo window, and the badge moves from mail.changed", async () => {
  const mailpit = await instance(page, "mailpit");
  const ui = mailpit?.ports.find((p) => p.name === "http")?.port;
  expect(ui, JSON.stringify(mailpit?.ports)).toBeTruthy();
  await go(page, "/mail");
  await expect(page.getByTestId("page-mail")).toBeVisible();
  await scan(page, "/mail (empty)");

  // The first count of a run is a silent baseline: let the watcher take it, then inject.
  await page.waitForTimeout(4_000);
  const injected = await fetch(`http://127.0.0.1:${String(ui)}/fake/inject?count=3&subject=Live%20hello`, {
    method: "POST",
  });
  expect(injected.ok).toBe(true);
  const badge = page.getByTestId("badge-mail");
  await expect(badge).toHaveText("3", { timeout: 20_000 });
  // The list follows the same event (one refetch per burst).
  await expect(page.locator("[data-mail-row]")).toHaveCount(3, { timeout: 20_000 });
  await scan(page, "/mail (list)");

  // Read one: the pane shows its subject and the message is read on the daemon.
  await page.locator("[data-mail-row]").first().click();
  await expect(page.getByTestId("mail-pane")).toContainText("Live hello");
  await expect
    .poll(async () => (await call<{ unread: number }>(page, "mail.status")).unread, { timeout: 15_000 })
    .toBe(2);
  await expect(badge).toHaveText("2", { timeout: 15_000 });
  await scan(page, "/mail (message)");

  // Delete it: it disappears at once, the call goes out after the 5 s window.
  await page.getByTestId("mail-pane").getByRole("button", { name: "Delete" }).click();
  await expect(page.locator("[data-mail-row]")).toHaveCount(2);
  expect((await call<{ messages: number }>(page, "mail.status")).messages).toBe(3);
  await expect
    .poll(async () => (await call<{ messages: number }>(page, "mail.status")).messages, {
      timeout: 20_000,
    })
    .toBe(2);
  await expect(page.locator("[data-mail-row]")).toHaveCount(2);
  // Another client changes the mailbox: the badge follows with no navigation.
  // (Which messages are unread by now depends on what the screen auto-selected after the
  // delete, so the badge is compared with the daemon's own count.)
  const unreadNow = async () => (await call<{ unread: number }>(page, "mail.status")).unread;
  const before = await unreadNow();
  await fetch(`http://127.0.0.1:${String(ui)}/fake/inject?count=1&subject=Late`, { method: "POST" });
  await expect.poll(unreadNow, { timeout: 20_000 }).toBe(before + 1);
  await expect(badge).toHaveText(String(before + 1), { timeout: 20_000 });
});

test("2 terminal: typed input runs in a real shell and exit shows the ended banner", async () => {
  await go(page, "/terminal");
  await expect(page.getByTestId("page-terminal")).toBeVisible();
  await page.getByRole("button", { name: "New terminal" }).click();
  await expect
    .poll(async () => (await call<{ terminals: unknown[] }>(page, "terminal.list")).terminals.length, {
      timeout: 30_000,
    })
    .toBeGreaterThan(0);
  await expect(page.locator(".xterm").first()).toBeVisible();
  await scan(page, "/terminal");

  // Echo: the shell writes the marker into a file, which only happens if the keys arrived.
  const proof = join(root(), "tmp", "terminal-echo.txt");
  await page.locator(".xterm").first().click();
  await page.waitForTimeout(1_500); // the prompt
  await page.keyboard.type(`echo FORGE_E3_ECHO > "${proof}"`);
  await page.keyboard.press("Enter");
  await expect
    .poll(() => (existsSync(proof) ? readFileSync(proof).toString("latin1").replace(/\0/g, "") : ""), {
      timeout: 20_000,
    })
    .toContain("FORGE_E3_ECHO");
  // The emulator received output too (the daemon counts it in the ring).
  const sessions = await call<{ terminals: { exit_code?: number | null }[] }>(page, "terminal.list");
  expect(sessions.terminals.length).toBe(1);

  await page.keyboard.type("exit");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("terminal-exited")).toContainText("Process ended", { timeout: 20_000 });
});

test("3 logs: the site target reads the fixture access log with its access columns", async () => {
  const queue = need("FORGE_E2E_QUEUE");
  const job = await call<Job>(page, "sites.up", { dir: queue.replace(/\\/g, "/") });
  const done = await finish(page, job);
  expect(done.state, JSON.stringify(done.error)).toBe("done");
  const access = (t: string, host: string, status: number, uri: string) =>
    `{"t":"${t}","host":"${host}","s":${String(status)},"rt":0.012,"ut":"0.010","b":512,"m":"GET","u":"${uri}","up":"127.0.0.1:19083"}\n`;
  writeFileSync(
    join(root(), "logs", "nginx", "access.json"),
    access("1761652800.500", "queue.test", 200, "/index.php") +
      access("1761652801.000", "other.test", 200, "/elsewhere") +
      access("1761652802.000", "www.queue.test", 404, "/missing") +
      access("1761652804.000", "queue.test", 503, "/down"),
  );

  await go(page, "/logs?target=site:queue.test");
  await expect(page.getByTestId("page-logs")).toBeVisible();
  const lines = page.getByTestId("log-lines");
  await expect(lines).toContainText("/index.php", { timeout: 20_000 });
  await expect(lines).toContainText("/missing");
  await expect(lines).toContainText("/down");
  // Only this site's hosts (the alias included).
  await expect(lines).not.toContainText("/elsewhere");
  const statuses = await lines.locator('[data-col="status"]').allTextContents();
  expect(statuses.map((s) => s.trim())).toEqual(["200", "404", "503"]);
  await scan(page, "/logs (site)");
});

test("4 doctor: Fix installs the CA into the (memory) store and the row changes in place", async () => {
  // Make a CA that exists but is not trusted: install it, then remove it again.
  const installed = await finish(page, await call<Job>(page, "tls.ca_install", { scope: "current_user" }));
  expect(installed.state, JSON.stringify(installed.error)).toBe("done");
  const removed = await finish(page, await call<Job>(page, "tls.ca_uninstall", { scope: "current_user" }));
  expect(removed.state, JSON.stringify(removed.error)).toBe("done");

  await go(page, "/diagnostics");
  await expect(page.getByTestId("page-diagnostics")).toBeVisible();
  await page.getByRole("button", { name: "Run diagnostics" }).click();
  await expect(page.getByTestId("doctor-summary")).toBeVisible({ timeout: 60_000 });
  const row = page.locator('[data-check="tls.ca_trusted"]');
  await expect(row).toHaveAttribute("data-status", "warn");
  await scan(page, "/diagnostics");
  const other = page.locator('[data-check="tls.ca"]');
  await expect(other).toHaveAttribute("data-status", "ok");
  await row.getByRole("button", { name: "Fix" }).click();
  await expect(row).toHaveAttribute("data-status", "ok", { timeout: 30_000 });
  // The daemon agrees (the memory store now trusts the CA).
  const status = await call<{ trusted_current_user: boolean }>(page, "tls.ca_status");
  expect(status.trusted_current_user).toBe(true);
});

test("5 settings: a config change made by another client shows in the open screen", async () => {
  await go(page, "/settings");
  await page.getByTestId("settings-nav-editor").click();
  const input = page.getByTestId("setting-editor.command").getByRole("textbox", {
    name: "Editor command",
  });
  await expect(input).toBeVisible();
  await scan(page, "/settings (editor)");
  const value = "code -g {path}:{line} --live-e3";
  await call(page, "system.config_set", { key: "editor.command", value });
  await expect(input).toHaveValue(value, { timeout: 15_000 });
  // The file on disk has it too.
  expect(readFileSync(join(root(), "etc", "forgebox.toml"), "utf8")).toContain("--live-e3");
});

test("6 databases: create, export with cancel, export, import", async () => {
  await go(page, "/databases");
  await expect(page.getByTestId("page-databases")).toBeVisible();
  await expect(page.locator('[data-instance="mysql/default"]')).toContainText("Running", {
    timeout: 30_000,
  });
  await scan(page, "/databases");

  // Create (the fake client keeps its "server" in databases.txt).
  const form = page.getByTestId("create-database");
  await form.getByLabel("Name").fill("shop");
  await form.getByRole("button", { name: "Create" }).click();
  const row = page.locator('[data-database="shop"]');
  await expect(row).toBeVisible({ timeout: 30_000 });
  expect(readFileSync(join(mysqlBin(), "databases.txt"), "utf8")).toContain("shop");

  // Export, cancelled half way: 64 MiB at ~200 ms per MiB leaves time to click Cancel.
  writeFileSync(join(mysqlBin(), "dump.txt"), "67108864 200");
  await row.getByRole("button", { name: "Export" }).click();
  const transfers = page.getByTestId("transfers");
  await expect(transfers).toContainText("written", { timeout: 30_000 });
  await scan(page, "/databases (export running)");
  await transfers.getByRole("button", { name: "Cancel" }).click();
  await expect(transfers).toContainText("Export cancelled.", { timeout: 30_000 });
  // No file left by the cancelled export (neither the dump nor a `.partial`).
  const backups = join(root(), "backups", "mysql", "default");
  const leftovers = () => (existsSync(backups) ? readdirSync(backups) : []);
  expect(leftovers().filter((f) => f.startsWith("shop-"))).toEqual([]);
  await transfers.getByRole("button", { name: "Dismiss" }).click();

  // Export that completes.
  writeFileSync(join(mysqlBin(), "dump.txt"), "4194304 10");
  await row.getByRole("button", { name: "Export" }).click();
  await expect(transfers).toContainText("Exported to", { timeout: 60_000 });
  const files = leftovers().filter((f) => f.startsWith("shop-") && f.endsWith(".sql.zst"));
  expect(files.length).toBe(1);
  await transfers.getByRole("button", { name: "Dismiss" }).click();

  // Import a plain .sql: the fake restore copies stdin to restored.bin.
  const dump = join(root(), "tmp", "import-me.sql");
  const body = "INSERT INTO `t` VALUES (1,'live');\n".repeat(40_000);
  writeFileSync(dump, body);
  await row.getByRole("button", { name: "Import" }).click();
  const sheet = page.getByTestId("import-sheet");
  await sheet.getByLabel("Dump file").fill(dump);
  await sheet.getByRole("button", { name: "Import" }).click();
  await expect(transfers).toContainText("Import finished.", { timeout: 60_000 });
  const restored = join(mysqlBin(), "restored.bin");
  await expect.poll(() => existsSync(restored) && statSync(restored).size, { timeout: 10_000 }).toBe(
    Buffer.byteLength(body),
  );
  await scan(page, "/databases (done)");
});

test("parity: every [data-action] of the E3 routes has a validated forge command", () => {
  const routes = new Set(scanned.map((s) => s.route));
  console.log(`PARITY ${String(scanned.length)} actions on ${String(routes.size)} views`);
  expect(scanned.length).toBeGreaterThan(5);
  const problems = parityProblems(scanned, forgeAccepts(need("FORGE_E2E_FORGE")));
  expect(problems).toEqual([]);
});
