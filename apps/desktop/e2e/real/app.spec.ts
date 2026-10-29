// Real-window e2e against Forgebox.exe + forgeboxd on a temp portable root (ADR-0036).
// scripts/e2e-desktop.ps1 starts the app and exports:
//   FORGE_E2E_CDP_PORT   WebView2 remote debugging port of the first instance
//   FORGE_E2E_EXE        Forgebox.exe (a second instance is launched from it)
//   FORGE_E2E_ROOT       temp portable root;  FORGE_E2E_GUI_PID  pid of the first instance
//   FORGE_E2E_PROBE      scripts/qa/window-probe.ps1
// The M2 screens do not exist yet at this task: the connection and the services are driven
// through the same Tauri commands the state layer (src/state) uses, and the status bar is
// asserted too as soon as it exposes `data-testid="connection-status"` with `data-status`.
import { execFileSync, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { chromium, expect, test, type Browser, type Page } from "@playwright/test";

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run scripts/e2e-desktop.ps1`);
  return value;
}

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
function ulid(): string {
  let time = "";
  let rest = Date.now();
  for (let i = 0; i < 10; i += 1) {
    time = CROCKFORD.charAt(rest % 32) + time;
    rest = Math.floor(rest / 32);
  }
  let random = "";
  for (let i = 0; i < 16; i += 1) random += CROCKFORD.charAt(Math.floor(Math.random() * 32));
  return time + random;
}

interface Internals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  transformCallback: (cb: (message: unknown) => void, once?: boolean) => number;
}

async function invoke<T>(page: Page, cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(
    ([c, a]) =>
      (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__.invoke(c, a),
    [cmd, args] as const,
  ) as Promise<T>;
}

const call = <T>(page: Page, method: string, params: Record<string, unknown> = {}) =>
  invoke<T>(page, "ipc_call", { context: "local", method, params, request_id: ulid() });

interface ContextState {
  status: string;
  hello: { local: boolean } | null;
}
const state = (page: Page) => invoke<ContextState>(page, "ipc_state", { context: "local" });

interface InstanceRow {
  tool: string;
  name: string;
  state: string;
}
async function mailpitState(page: Page): Promise<string | undefined> {
  const status = await call<{ instances: InstanceRow[] }>(page, "services.status");
  return status.instances.find((i) => i.tool === "mailpit")?.state;
}

function daemonPid(root: string): number | null {
  try {
    const lock = JSON.parse(
      readFileSync(join(root, "state", "locks", "forgeboxd.lock"), "utf8"),
    ) as { pid?: unknown };
    return typeof lock.pid === "number" ? lock.pid : null;
  } catch {
    return null;
  }
}

function probe(pid: number): { minimized: boolean; foreground: boolean; visible: boolean } {
  const out = execFileSync(
    "powershell",
    ["-NoProfile", "-File", need("FORGE_E2E_PROBE"), "-Action", "Info", "-ProcessId", String(pid)],
    { encoding: "utf8" },
  );
  return JSON.parse(out.trim().split(/\r?\n/).at(-1) ?? "{}") as ReturnType<typeof probe>;
}

let browser: Browser;
let page: Page;
const root = () => need("FORGE_E2E_ROOT");

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${need("FORGE_E2E_CDP_PORT")}`);
  const pages = browser.contexts().flatMap((c) => c.pages());
  const found = pages.find((p) => p.url().includes("tauri.localhost"));
  if (!found) throw new Error(`no tauri.localhost page among ${pages.map((p) => p.url()).join()}`);
  page = found;
});

test.afterAll(async () => {
  await browser.close().catch(() => undefined);
});

test("the daemon auto-starts and the connection reports connected", async () => {
  await expect.poll(async () => (await state(page)).status, { timeout: 60_000 }).toBe("connected");
  expect((await state(page)).hello?.local).toBe(true);
  expect(daemonPid(root())).not.toBeNull();
  const bar = page.getByTestId("connection-status");
  if ((await bar.count()) > 0) await expect(bar).toHaveAttribute("data-status", "connected");
});

test("mailpit stop and start reach the UI through service events", async () => {
  // Subscribe first, so no event of the stop/start is missed. Frames are kept as parsed objects.
  await page.evaluate(async () => {
    const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
    const w = window as unknown as { __e2eEvents: unknown[] };
    w.__e2eEvents = [];
    const id = internals.transformCallback((message) => {
      w.__e2eEvents.push(message);
    });
    await internals.invoke("ipc_subscribe", {
      context: "local",
      method: "events.subscribe",
      params: {},
      on_frame: `__CHANNEL__:${String(id)}`,
    });
  });
  // The daemon starts mailpit by itself at boot, so it is running here: stop it first.
  await expect.poll(() => mailpitState(page), { timeout: 30_000 }).toBe("running");

  const serviceEvents = () =>
    page.evaluate(() => {
      const frames = (window as unknown as { __e2eEvents: unknown[] }).__e2eEvents;
      const out: { kind: string; name: string; state?: string | undefined }[] = [];
      const walk = (v: unknown): void => {
        if (Array.isArray(v)) v.forEach(walk);
        else if (v && typeof v === "object") {
          const o = v as Record<string, unknown>;
          if (typeof o.kind === "string" && o.kind.startsWith("service.")) {
            const p = (o.payload ?? o) as Record<string, unknown>;
            out.push({ kind: o.kind, name: String(p.name), state: p.state as string | undefined });
          } else Object.values(o).forEach(walk);
        }
      };
      frames.forEach(walk);
      return out;
    });
  const has = async (kind: string) =>
    (await serviceEvents()).some((e) => e.kind === kind && e.name === "mailpit/default");

  await call(page, "services.stop", { targets: ["mailpit/default"] });
  await expect.poll(() => has("service.stopped"), { timeout: 30_000 }).toBe(true);
  await expect.poll(() => mailpitState(page), { timeout: 30_000 }).toBe("stopped");

  await call(page, "services.start", { targets: ["mailpit/default"] });
  await expect.poll(() => has("service.running"), { timeout: 30_000 }).toBe(true);
  await expect.poll(() => mailpitState(page), { timeout: 30_000 }).toBe("running");
});

test("killing forgeboxd shows reconnecting and then reconnects within 10 s", async () => {
  const before = daemonPid(root());
  expect(before).not.toBeNull();
  execFileSync("taskkill", ["/PID", String(before), "/T", "/F"], { stdio: "ignore" });
  const killedAt = Date.now();
  const seen: string[] = [];
  let reconnectedAt = 0;
  while (Date.now() - killedAt < 30_000) {
    const status = (await state(page)).status;
    if (seen.at(-1) !== status) seen.push(status);
    if (status === "connected" && seen.length > 1) {
      reconnectedAt = Date.now();
      break;
    }
    await page.waitForTimeout(40);
  }
  expect(seen, `statuses seen: ${seen.join(" > ")}`).toContain("reconnecting");
  expect(reconnectedAt, "never reconnected").toBeGreaterThan(0);
  expect(reconnectedAt - killedAt, `statuses: ${seen.join(" > ")}`).toBeLessThan(10_000);
  const after = daemonPid(root());
  expect(after).not.toBeNull();
  expect(after).not.toBe(before);
  const bar = page.getByTestId("connection-status");
  if ((await bar.count()) > 0) await expect(bar).toHaveAttribute("data-status", "connected");
});

test("a second instance exits and focuses the first window", async () => {
  const pid = Number(need("FORGE_E2E_GUI_PID"));
  await invoke(page, "plugin:window|minimize", { label: "main" });
  await expect.poll(() => probe(pid).minimized, { timeout: 5_000 }).toBe(true);

  const second = spawn(need("FORGE_E2E_EXE"), [], { env: process.env, stdio: "ignore" });
  const code = await new Promise<number | null>((ok) => {
    const timer = setTimeout(() => {
      second.kill();
      ok(null);
    }, 10_000);
    second.on("exit", (c) => {
      clearTimeout(timer);
      ok(c);
    });
  });
  expect(code, "the second instance must exit by itself with 0").toBe(0);
  await expect.poll(() => probe(pid).minimized, { timeout: 5_000 }).toBe(false);
  expect(probe(pid).visible).toBe(true);
});
