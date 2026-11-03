// Acceptance of M2-E2-T12: states, grouped list, failure card, job feedback, typed-name removal.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../../App";
import { createTestRouter } from "../../app/router";
import { warmAppModules, renderApp, type AppHarness } from "../../app/testApp";
import { createFakeBridge, fakeError, fakeJob, type FakeScenario } from "../../ipc/fakeBridge";
import type { Ack, ServiceStatusDto } from "../../ipc/generated";
import { StateLayer } from "../../state/layer";
import { manualPageActivity } from "../../state/page";
import {
  IDS,
  instance,
  populatedData,
  stateScenario,
  timerFrameClock,
  tool,
  type ScenarioData,
} from "../../state/testFixtures";

const OK: Ack = { ok: true, message: null };
const STATUS: ServiceStatusDto = { instances: [], pools: [] };

function categorized(): ScenarioData {
  const data = populatedData();
  const mysql = data.instances[0];
  if (mysql !== undefined) mysql.version = "8.4.3";
  data.tools = [
    tool("mysql", { name: "MySQL", category: "database", current: "8.4.3", has_service: true }),
    tool("redis", { name: "Redis", category: "cache", has_service: true }),
    tool("php", { name: "PHP", category: "runtime" }),
  ];
  return data;
}

function failedPayload(id: string, name: string) {
  return {
    v: 1,
    instance_id: id,
    name,
    tool: name.split("/")[0],
    state: "failed",
    pid: null,
    strategy: null,
    exit_code: null,
    error: { code: "port_in_use", message: "port 6379 is in use" },
    reason: { kind: "port_in_use", port: 6379, pid: 4321, exe: "redis-server.exe" },
  };
}

interface Mounted {
  fake: ReturnType<typeof createFakeBridge>;
  layer: StateLayer;
  router: ReturnType<typeof createTestRouter>;
}

/** Mounts the app on /services WITHOUT waiting for the snapshots (loading and error states). */
async function mount(data: ScenarioData, extra: FakeScenario = {}): Promise<Mounted> {
  await warmAppModules();
  window.innerWidth = 1280;
  const fake = createFakeBridge(stateScenario(data, extra));
  const layer = new StateLayer(fake, {
    page: manualPageActivity(),
    frame: timerFrameClock(),
    rttIntervalMs: 0,
    snapshotDebounceMs: 0,
    invalidateDebounceMs: 0,
  });
  const router = createTestRouter("/services");
  render(<App layer={layer} router={router} />);
  return { fake, layer, router };
}

/** Waits for an element matching `selector` under `root`. */
async function found(selector: string, root: ParentNode = document): Promise<HTMLElement> {
  return waitFor(() => {
    const el = root.querySelector<HTMLElement>(selector);
    if (el === null) throw new Error(`no element matches ${selector}`);
    return el;
  });
}

function rowOf(target: string): Promise<HTMLElement> {
  return found(`[data-service-row][data-target="${target}"]`);
}

async function openSheet(user: ReturnType<typeof userEvent.setup>, target: string) {
  // The row button is named by its visible text (tool, version and instance).
  const row = await rowOf(target);
  const button = within(row).getAllByRole("button")[0];
  if (button === undefined) throw new Error("no details button");
  await user.click(button);
  return screen.findByTestId("service-sheet");
}

async function publish(h: AppHarness, kind: string, payload: unknown) {
  await act(async () => {
    h.fake.publish(kind, payload);
    h.fake.flush();
    await h.layer.runtime("local").settled();
  });
}

describe("states", () => {
  it("shows a skeleton while the first snapshot loads, then the list", async () => {
    const data = categorized();
    const slow: FakeScenario = {
      handlers: {
        "services.status": async () => {
          await new Promise((resolve) => setTimeout(resolve, 1500));
          return { instances: data.instances, pools: data.pools };
        },
      },
    };
    await mount(data, slow);
    await waitFor(() => {
      expect(document.querySelector("[data-async-view=loading]")).not.toBeNull();
    });
    expect(await screen.findByRole("heading", { name: "Databases" })).toBeInTheDocument();
    expect(document.querySelector("[data-async-view]")).toHaveAttribute("data-async-view", "ready");
  });

  it("explains an empty stack and points to the Catalog", async () => {
    const data = categorized();
    data.instances = [];
    data.pools = [];
    const user = userEvent.setup();
    const h = await renderApp({ path: "/services", data });
    expect(await screen.findByText("No services yet")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open the Catalog" }));
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe("/catalog");
    });
  });

  it("shows the cause of a failed snapshot with a way to try again", async () => {
    const data = categorized();
    const { fake } = await mount(data, {
      handlers: {
        "services.status": () => fakeError("internal", "boom", "fake_error", {}),
      },
    });
    const alert = await found("[data-error-notice]");
    expect(alert).toHaveAttribute("data-error-code", "internal");
    expect(within(alert).getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(fake.calls.some((call) => call.method === "services.status")).toBe(true);
  });

  it("says the daemon is not connected instead of an empty list", async () => {
    await renderApp({ scenario: "daemon-down", path: "/services" });
    expect(await screen.findByText("No connection to the daemon")).toBeInTheDocument();
    expect(document.querySelector("[data-async-view]")).toHaveAttribute(
      "data-async-view",
      "disconnected",
    );
  });
});

describe("list", () => {
  it("groups by category and shows pools with busy/total workers", async () => {
    const data = categorized();
    const pool = data.pools[0];
    if (pool !== undefined) {
      pool.busy_workers = 1;
      pool.workers = 4;
    }
    await renderApp({ path: "/services", data });
    const headings = await screen.findAllByRole("heading", { level: 2 });
    expect(headings.map((h) => h.textContent)).toEqual(["Databases", "Cache", "Runtimes"]);
    const mysql = await rowOf("mysql/default");
    expect(within(mysql).getByText("mysql 8.4.3")).toBeInTheDocument();
    expect(within(mysql).getByTestId("row-state")).toHaveTextContent(/^Running/);
    const redis = await rowOf("redis/default");
    expect(within(redis).getByTestId("row-state")).toHaveTextContent("Stopped");
    const php = await rowOf("php@8.3");
    expect(within(php).getByText("1/4 workers", { exact: false })).toBeInTheDocument();
  });

  it("renders the tool icon only as an img data URI", async () => {
    const svg =
      '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><script>x</script></svg>';
    await renderApp({
      path: "/services",
      data: categorized(),
      extra: {
        // The fake answers tools.icon with not_found by default: give this tool an icon.
        handlers: { "tools.icon": (params) => ({ tool: params.tool, svg, sha256: "00" }) },
      },
    });
    const row = await rowOf("mysql/default");
    const img = await found("img", row);
    expect(img.getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(document.querySelector("svg script")).toBeNull();
  });

  it("opens the sheet of ?instance=<id> (palette deep link)", async () => {
    await renderApp({ path: `/services?instance=${IDS.mysql}`, data: categorized() });
    expect(await screen.findByTestId("service-sheet")).toHaveAttribute(
      "data-target",
      "mysql/default",
    );
  });
});

describe("failure", () => {
  it("updates only the failed row, with cause, actions and the CLI to change the port", async () => {
    const user = userEvent.setup();
    const data = categorized();
    const h = await renderApp({ path: "/services", data });
    const mysqlBefore = await rowOf("mysql/default");
    const redisBefore = await rowOf("redis/default");
    expect(redisBefore).toHaveAttribute("data-state", "stopped");

    await publish(h, "service.failed", failedPayload(IDS.redis, "redis/default"));

    await waitFor(() => {
      expect(redisBefore).toHaveAttribute("data-state", "failed");
    });
    // The other row is the same node in the same state: nothing else was touched.
    expect(await rowOf("mysql/default")).toBe(mysqlBefore);
    expect(mysqlBefore).toHaveAttribute("data-state", "running");
    expect(within(redisBefore).getByTestId("row-state")).toHaveTextContent("Failed");

    await openSheet(user, "redis/default");
    const card = await screen.findByTestId("failure-card");
    expect(card).toHaveTextContent("Port 6379 is in use by redis-server.exe (PID 4321).");
    await user.click(within(card).getByRole("button", { name: "Change port" }));
    const field = within(screen.getByTestId("change-port")).getByLabelText("New main port");
    await user.type(field, "6380");
    expect(
      await within(card).findByText("forge service set redis/default ports.main=6380"),
    ).toBeInTheDocument();
    // The Kill action is offered with the holder named; `services.kill_port_holder` is
    // not implemented yet, so the capability gate disables it with the reason.
    const kill = within(card).getByRole("button", { name: /End redis-server\.exe/ });
    expect(kill).toBeDisabled();
  });

  it("changes the port through services.set with string settings", async () => {
    const user = userEvent.setup();
    const data = categorized();
    const redis = data.instances[1];
    if (redis !== undefined) {
      redis.state = "failed";
      redis.last_error = {
        code: "port_in_use",
        message: "in use",
        details: { kind: "port_in_use", port: 6379, pid: 4321, exe: "redis-server.exe" },
      };
    }
    const h = await renderApp({
      path: "/services",
      data,
      extra: {
        handlers: {
          "services.set": () => fakeJob<ServiceStatusDto>({ outcome: { done: STATUS } }),
        },
      },
    });
    await openSheet(user, "redis/default");
    const card = await screen.findByTestId("failure-card");
    await user.click(within(card).getByRole("button", { name: "Change port" }));
    await user.type(within(card).getByLabelText("New main port"), "6380");
    await user.click(
      within(screen.getByTestId("change-port")).getByRole("button", { name: "Change port" }),
    );
    await waitFor(() => {
      const call = h.fake.calls.find((c) => c.method === "services.set");
      expect(call?.params).toEqual({
        target: "redis/default",
        settings: { "ports.main": "6380" },
      });
    });
  });
});

describe("start and stop", () => {
  it("shows the running text at once, then the job result, with the CLI of each", async () => {
    const user = userEvent.setup();
    const h = await renderApp({
      path: "/services",
      data: categorized(),
      extra: {
        jobStepMs: 40,
        handlers: {
          "services.start": () =>
            fakeJob<ServiceStatusDto>({
              steps: [{ pct: 50, message: "waiting for the port" }],
              outcome: { done: STATUS },
            }),
          "services.stop": () => fakeJob<ServiceStatusDto>({ outcome: { done: STATUS } }),
        },
      },
    });
    const redis = await rowOf("redis/default");
    const start = within(redis).getByRole("button", { name: "Start" });
    expect(start.closest("[data-action]")).toHaveAttribute(
      "data-forge-cli",
      "forge start redis/default",
    );
    const mysql = await rowOf("mysql/default");
    expect(
      within(mysql).getByRole("button", { name: "Stop" }).closest("[data-action]"),
    ).toHaveAttribute("data-forge-cli", "forge stop mysql/default");

    await user.click(start);
    // Same frame as the click: the optimistic text, before any answer.
    expect(within(redis).getByText("Starting…")).toBeInTheDocument();
    await publish(h, "service.starting", {
      ...failedPayload(IDS.redis, "redis/default"),
      state: "starting",
      error: null,
      reason: null,
    });
    await waitFor(() => {
      expect(within(redis).getByTestId("row-state")).toHaveTextContent("Starting");
    });
    await publish(h, "service.running", {
      ...failedPayload(IDS.redis, "redis/default"),
      state: "running",
      pid: 99,
      error: null,
      reason: null,
    });
    await waitFor(() => {
      expect(within(redis).getByTestId("row-state")).toHaveTextContent(/^Running/);
    });
    await waitFor(() => {
      expect(within(redis).getByText("Started")).toBeInTheDocument();
    });
    expect(h.fake.calls.find((c) => c.method === "services.start")?.params).toEqual({
      targets: ["redis/default"],
    });
  });
});

describe("instances", () => {
  it("keeps Remove disabled until the exact name is typed, then sends confirm and delete_data", async () => {
    const user = userEvent.setup();
    const h = await renderApp({
      path: "/services",
      data: categorized(),
      extra: { handlers: { "services.remove": () => fakeJob<Ack>({ outcome: { done: OK } }) } },
    });
    await openSheet(user, "redis/default");
    await user.click(screen.getByRole("tab", { name: "Instances" }));
    await user.click(await screen.findByRole("button", { name: "Remove redis/default" }));
    const box = await screen.findByTestId("remove-confirm");
    const confirm = within(box).getByRole("button", { name: "Remove instance" });
    expect(confirm).toBeDisabled();
    const input = within(box).getByLabelText("Type default to confirm");
    await user.type(input, "defaul");
    expect(confirm).toBeDisabled();
    await user.type(input, "t");
    expect(confirm).toBeEnabled();
    await user.type(input, "x");
    expect(confirm).toBeDisabled();
    await user.type(input, "{Backspace}");
    await user.click(within(box).getByRole("switch", { name: "Also delete its data" }));
    await user.click(confirm);
    await waitFor(() => {
      const call = h.fake.calls.find((c) => c.method === "services.remove");
      expect(call?.params).toEqual({
        instance: "redis/default",
        delete_data: true,
        confirm: true,
      });
    });
  });

  it("validates the name of a new instance and creates it with the chosen options", async () => {
    const user = userEvent.setup();
    const data = categorized();
    data.tools = [
      tool("mysql", { name: "MySQL", category: "database", has_service: true }),
      ...data.tools.slice(1),
    ];
    const h = await renderApp({
      path: "/services",
      data,
      extra: {
        handlers: {
          "services.create": () =>
            fakeJob({ outcome: { done: instance(IDS.redis, "mysql/legacy") } }),
        },
      },
    });
    await openSheet(user, "mysql/default");
    await user.click(screen.getByRole("tab", { name: "Instances" }));
    const form = await screen.findByTestId("create-instance");
    const submit = within(form).getByRole("button", { name: "Create instance" });
    expect(submit).toBeDisabled();
    const name = within(form).getByLabelText("Name");
    await user.type(name, "default");
    await waitFor(() => {
      expect(
        within(form).getAllByText("An instance with this name already exists.").length,
      ).toBeGreaterThan(0);
    });
    expect(submit).toBeDisabled();
    await user.clear(name);
    await user.type(name, "legacy");
    await user.type(within(form).getByLabelText("Main port (optional)"), "3307");
    await user.click(within(form).getByRole("switch", { name: "Start with Forgebox" }));
    expect(submit).toBeEnabled();
    expect(submit.closest("[data-action]")).toHaveAttribute("data-forge-cli");
    await user.click(submit);
    await waitFor(() => {
      const call = h.fake.calls.find((c) => c.method === "services.create");
      expect(call?.params).toEqual({
        tool: "mysql@8.4.3",
        name: "legacy",
        port: 3307,
        autostart: true,
      });
    });
  });
});

describe("logs and timeline", () => {
  it("tails the log: backlog, live lines, filter, pause and resume", async () => {
    const user = userEvent.setup();
    const h = await renderApp({
      path: "/services",
      data: categorized(),
      extra: {
        handlers: {
          "logs.read": () => ({
            target: "mysql/default",
            last_seq: 2,
            lines: [
              { seq: 1, ts: null, source: "stdout", level: "info", text: "ready for connections" },
              { seq: 2, ts: null, source: "stderr", level: "warn", text: "slow query" },
            ],
          }),
        },
      },
    });
    // Same seq domain as the backlog: the fake numbers the live lines from 1.
    act(() => {
      for (const text of ["ready for connections", "slow query"]) {
        h.fake.emitLog("mysql/default", { ts: null, source: "stdout", level: null, text });
      }
    });
    await openSheet(user, "mysql/default");
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    const log = await screen.findByRole("log");
    await waitFor(() => {
      expect(within(log).getByText("ready for connections")).toBeInTheDocument();
    });
    await waitFor(() => {
      expect(h.fake.openStreams()).toBeGreaterThan(1);
    });
    act(() => {
      h.fake.emitLog("mysql/default", {
        ts: null,
        source: "stdout",
        level: null,
        text: "accepted a client",
      });
      h.fake.flush();
    });
    await waitFor(() => {
      expect(within(log).getByText("accepted a client")).toBeInTheDocument();
    });
    await user.type(screen.getByLabelText("Filter"), "slow");
    expect(within(log).queryByText("ready for connections")).toBeNull();
    expect(within(log).getByText("slow query")).toBeInTheDocument();
    await user.clear(screen.getByLabelText("Filter"));

    await user.click(screen.getByRole("button", { name: "Pause" }));
    act(() => {
      h.fake.emitLog("mysql/default", {
        ts: null,
        source: "stdout",
        level: null,
        text: "while paused",
      });
      h.fake.flush();
    });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(within(log).queryByText("while paused")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Resume" }));
    expect(await within(log).findByText("while paused")).toBeInTheDocument();
  });

  it("lists the state changes of the instance, newest first", async () => {
    const user = userEvent.setup();
    const history = [
      { id: "01A", seq: 1, ts: "2026-10-28T10:00:00Z", kind: "service.starting", payload: {} },
      { id: "01B", seq: 2, ts: "2026-10-28T10:00:02Z", kind: "service.running", payload: {} },
    ];
    await renderApp({
      path: "/services",
      data: categorized(),
      extra: {
        handlers: {
          "events.list": () => ({ events: history, next_after: 2, last_seq: 2, has_more: false }),
        },
      },
    });
    await openSheet(user, "mysql/default");
    const timeline = await screen.findByTestId("timeline");
    const kinds = within(timeline)
      .getAllByRole("listitem")
      .map((li) => li.getAttribute("data-event-kind"));
    expect(kinds).toEqual(["service.running", "service.starting"]);
  });

  it("follows has_more so the timeline shows the newest events", async () => {
    const user = userEvent.setup();
    const ev = (seq: number, kind: string) => ({
      id: `E${String(seq)}`,
      seq,
      ts: "2026-10-28T10:00:00Z",
      kind,
      payload: {},
    });
    const first = Array.from({ length: 20 }, (_, i) => ev(i + 1, "service.starting"));
    const second = [ev(21, "service.running"), ev(22, "service.stopped")];
    await renderApp({
      path: "/services",
      data: categorized(),
      extra: {
        handlers: {
          "events.list": (params: { after_seq?: number }) =>
            (params.after_seq ?? 0) === 0
              ? { events: first, next_after: 20, last_seq: 22, has_more: true }
              : { events: second, next_after: 22, last_seq: 22, has_more: false },
        },
      },
    });
    await openSheet(user, "mysql/default");
    const timeline = await screen.findByTestId("timeline");
    const kinds = within(timeline)
      .getAllByRole("listitem")
      .map((li) => li.getAttribute("data-event-kind"));
    expect(kinds).toHaveLength(12);
    expect(kinds.slice(0, 2)).toEqual(["service.stopped", "service.running"]);
  });
});

describe("palette", () => {
  it("has per-service entries: restart a running one and open its details", async () => {
    const user = userEvent.setup();
    const h = await renderApp({ path: "/services", data: categorized() });
    await user.keyboard("{Control>}k{/Control}");
    await screen.findByRole("dialog");
    await user.keyboard("mysql");
    expect(
      await screen.findByRole("option", { name: /Restart mysql\/default/ }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("option", { name: /Open details of mysql\/default/ }));
    await waitFor(() => {
      expect(h.router.state.location.search).toEqual({ instance: IDS.mysql });
    });
    expect(await screen.findByTestId("service-sheet")).toHaveAttribute(
      "data-target",
      "mysql/default",
    );
  });
});
