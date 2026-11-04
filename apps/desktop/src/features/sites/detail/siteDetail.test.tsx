// Acceptance of M2-E2-T16: the live PHP switch (happy path, not installed, failure), Processos
// from events, Diagnóstico with copyable fixes and the tab slot registry.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configHandlers } from "../../../../qa/fixtures/scenarios/config";
import { siteProcess } from "../../../../qa/fixtures/scenarios/data";
import {
  QA_BROKEN_SERIES,
  seedPhp,
  siteDetailHandlers,
} from "../../../../qa/fixtures/scenarios/siteDetail";
import { renderApp, type AppHarness } from "../../../app/testApp";
import { registerSiteTab } from "../../../app/slots";
import { fakeJob, type FakeHandlers } from "../../../ipc/fakeBridge";
import { populatedData, site as siteDto, type ScenarioData } from "../../../state/testFixtures";
import { CHANGE_WAIT_MS } from "./usePhpSwitch";

/** The value or a failing test (no non-null assertions). */
function must<T>(value: T | null | undefined): T {
  if (value === null || value === undefined) throw new Error("expected a value");
  return value;
}

const SITE_ID = "01JSITE00000000000000000LJA";
let harness: AppHarness | null = null;

afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
  vi.restoreAllMocks();
});

function detailData(): ScenarioData {
  const data = populatedData();
  data.sites = [
    siteDto(SITE_ID, "loja.test", {
      driver: "laravel",
      aliases: ["www.loja.test"],
      processes: [
        { ...siteProcess(1, "web", "php artisan serve", "running", 6100), site_id: SITE_ID },
        { ...siteProcess(1, "queue", "php artisan queue:work", "stopped", null), site_id: SITE_ID },
      ],
    }),
  ];
  data.tools = [...data.tools, { ...must(populatedData().tools[0]), id: "php", name: "PHP" }];
  seedPhp(data);
  return data;
}

async function open(
  data: ScenarioData = detailData(),
  handlers: FakeHandlers = {},
  path = `/sites/${SITE_ID}`,
  heading = true,
): Promise<AppHarness> {
  harness = await renderApp({
    path,
    data,
    extra: {
      notImplemented: [],
      handlers: { ...configHandlers(data), ...siteDetailHandlers(data), ...handlers },
    },
  });
  if (heading) await screen.findByRole("heading", { level: 1 });
  return harness;
}

const phpCalls = () => harness?.fake.calls.filter((call) => call.method === "sites.php") ?? [];
const calls = (method: string) =>
  harness?.fake.calls.filter((call) => call.method === method) ?? [];

async function pickPhp(series: string) {
  const user = userEvent.setup();
  const trigger = await screen.findByRole("combobox", { name: "PHP version" });
  await user.click(trigger);
  // The providers' series arrive after the first paint (`tools.releases`).
  const option = await screen.findByRole("option", { name: new RegExp(`^${series}`) });
  await user.click(option);
  return user;
}

describe("Geral", () => {
  it("shows host, aliases, folder, driver, server and the certificate status", async () => {
    await open();
    const general = await screen.findByTestId("site-general");
    expect(within(general).getByText("www.loja.test")).toBeInTheDocument();
    expect(within(general).getByText("C:/Forgebox/www/loja.test")).toBeInTheDocument();
    expect(within(general).getByText("laravel")).toBeInTheDocument();
    expect(within(general).getByText("nginx")).toBeInTheDocument();
    await waitFor(() => {
      expect(within(general).getByTestId("cert-status")).toHaveAttribute("data-tone", "success");
    });
    expect(await screen.findByRole("combobox", { name: "PHP version" })).toHaveTextContent("8.3");
  });

  it("opens the project in the explorer through sites.open", async () => {
    await open();
    await userEvent.setup().click(await screen.findByRole("button", { name: "Explorer" }));
    await waitFor(() => {
      expect(calls("sites.open")[0]?.params).toEqual({ site: SITE_ID, with: "explorer" });
    });
  });

  it("says when the site does not exist", async () => {
    await open(detailData(), {}, "/sites/01JSITE0000000000000000NONE", false);
    expect(await screen.findByText("Site not found")).toBeInTheDocument();
  });
});

describe("PHP switch", () => {
  it("goes idle -> switching with progress -> done only after job.done and site.php_changed", async () => {
    await open();
    const user = await pickPhp("8.2");
    const combo = screen.getByRole("combobox", { name: "PHP version" });
    // Optimistic: the selection moved, the control is locked and the badge says so.
    expect(combo).toHaveTextContent("8.2");
    expect(combo).toBeDisabled();
    expect(await screen.findByText("Switching to 8.2…")).toBeInTheDocument();
    expect(document.querySelector("[data-php-switch]")).toHaveAttribute(
      "data-php-switch",
      "switching",
    );
    expect(phpCalls()).toHaveLength(1);
    expect(phpCalls()[0]?.params).toEqual({ site: SITE_ID, php: "8.2" });
    await waitFor(() => {
      expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
    });
    expect(screen.getByRole("combobox", { name: "PHP version" })).toBeEnabled();
    expect(phpCalls()).toHaveLength(1);
    await user.keyboard("{Escape}");
  });

  it("does not finish on site.php_changed alone, and waits for it after job.done", async () => {
    const fakeData = detailData();
    const base = must(fakeData.sites[0]);
    const next = {
      ...base,
      php: { series: "8.2", exact: null, resolved: "8.2.30", upstream: "php_8_2" },
    };
    await open(fakeData, {
      "sites.php": (_params, ctx) => {
        setTimeout(() => {
          ctx.fake.publish("site.php_changed", {
            v: 1,
            site_id: SITE_ID,
            host: "loja.test",
            from: "8.3.36",
            to: "8.2.30",
          });
        }, 20);
        return fakeJob({ stepMs: 600, outcome: { done: next } });
      },
    });
    await pickPhp("8.2");
    await new Promise((resolve) => setTimeout(resolve, 250));
    // The event came, the job has not ended: still switching.
    expect(document.querySelector("[data-php-switch]")).toHaveAttribute(
      "data-php-switch",
      "switching",
    );
    await waitFor(
      () => {
        expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
      },
      { timeout: 3000 },
    );
  });

  it("changed version without site.php_changed stays switching until a refetch confirms it", async () => {
    const fakeData = detailData();
    const base = must(fakeData.sites[0]);
    const next = {
      ...base,
      php: { series: "8.2", exact: null, resolved: "8.2.30", upstream: "php_8_2" },
    };
    await open(fakeData, {
      "sites.php": () => {
        fakeData.sites.splice(0, 1, next);
        return fakeJob({ stepMs: 30, outcome: { done: next } });
      },
    });
    await pickPhp("8.2");
    await new Promise((resolve) => setTimeout(resolve, 600));
    // No event: not done yet.
    expect(document.querySelector("[data-php-switch]")).toHaveAttribute(
      "data-php-switch",
      "switching",
    );
    expect(screen.queryByText("PHP 8.2 is active")).not.toBeInTheDocument();
    await waitFor(
      () => {
        expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
      },
      { timeout: CHANGE_WAIT_MS + 2000 },
    );
    expect(calls("sites.list").length).toBeGreaterThan(1);
  });

  it("changed version never confirmed ends as a failure, not as success", async () => {
    const fakeData = detailData();
    const base = must(fakeData.sites[0]);
    const next = {
      ...base,
      php: { series: "8.2", exact: null, resolved: "8.2.30", upstream: "php_8_2" },
    };
    // The refetch keeps showing the old version.
    await open(fakeData, {
      "sites.php": () => fakeJob({ stepMs: 30, outcome: { done: next } }),
    });
    await pickPhp("8.2");
    await waitFor(
      () => {
        expect(document.querySelector("[data-php-switch-failure]")).not.toBeNull();
      },
      { timeout: CHANGE_WAIT_MS + 2000 },
    );
    expect(screen.queryByText("PHP 8.2 is active")).not.toBeInTheDocument();
  });

  it("unchanged resolved version finishes at job.done without waiting for an event", async () => {
    const fakeData = detailData();
    await open(fakeData, {
      "sites.php": () => fakeJob({ stepMs: 30, outcome: { done: must(fakeData.sites[0]) } }),
    });
    await pickPhp("8.2");
    await waitFor(
      () => {
        expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
      },
      { timeout: CHANGE_WAIT_MS - 500 },
    );
  });

  it("shows the equivalent CLI command of the switch with the host", async () => {
    await open();
    await pickPhp("8.2");
    expect(await screen.findByText("forge site php loja.test 8.2")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
    });
  });

  it("keeps the switch locked across a tab change and shows its outcome on return", async () => {
    const fakeData = detailData();
    const next = {
      ...must(fakeData.sites[0]),
      php: { series: "8.2", exact: null, resolved: "8.2.30", upstream: "php_8_2" },
    };
    // The job runs until the test ends it: a timer would race the tab round trip, which
    // takes longer than any fixed delay on a loaded machine (full vitest run).
    const h = await open(fakeData, {
      "sites.php": () => fakeJob({ hold: true, outcome: { done: next } }),
    });
    const user = await pickPhp("8.2");
    expect(await screen.findByText("Switching to 8.2…")).toBeInTheDocument();
    await user.click(await screen.findByRole("tab", { name: "Processes" }));
    await screen.findByRole("list", { name: "Processes" });
    await user.click(await screen.findByRole("tab", { name: "General" }));
    const combo = await screen.findByRole("combobox", { name: "PHP version" });
    expect(combo).toBeDisabled();
    expect(combo).toHaveTextContent("8.2");
    expect(screen.getByText("Switching to 8.2…")).toBeInTheDocument();
    expect(phpCalls()).toHaveLength(1);
    const job = must(h.fake.jobList().find((j) => j.name === "sites.php" && j.state === "running"));
    act(() => {
      h.fake.publish("site.php_changed", {
        v: 1,
        site_id: SITE_ID,
        host: "loja.test",
        from: "8.3.36",
        to: "8.2.30",
      });
      expect(h.fake.completeJob(job.id)).toBe(true);
      h.fake.flush();
    });
    await waitFor(() => {
      expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
    });
    expect(screen.getByRole("combobox", { name: "PHP version" })).toBeEnabled();
    expect(phpCalls()).toHaveLength(1);
  });

  it("job.failed restores the selection, shows the cause and sends no second request", async () => {
    const data = detailData();
    const php = must(data.tools.find((tool) => tool.id === "php"));
    php.installed.push({ ...must(php.installed[0]), version: `${QA_BROKEN_SERIES}.33` });
    await open(data);
    await pickPhp(QA_BROKEN_SERIES);
    expect(screen.getByRole("combobox", { name: "PHP version" })).toBeDisabled();
    const failure = await waitFor(() => {
      const el = document.querySelector("[data-php-switch-failure]");
      if (el === null) throw new Error("no failure yet");
      return el as HTMLElement;
    });
    expect(failure).toHaveTextContent("PHP stays at 8.3.");
    expect(within(failure).getByRole("alert")).toBeInTheDocument();
    const combo = screen.getByRole("combobox", { name: "PHP version" });
    expect(combo).toHaveTextContent("8.3");
    expect(combo).toBeEnabled();
    expect(phpCalls()).toHaveLength(1);
  });

  it("not installed: confirms with the size, installs, then retries; cancelling keeps the old PHP", async () => {
    await open();
    // Cancel first.
    const user = await pickPhp("8.4");
    const dialog = await screen.findByTestId("php-install-confirm");
    await waitFor(() => {
      expect(within(dialog).getByTestId("php-install-size")).toHaveTextContent("32 MB");
    });
    expect(within(dialog).getByText("forge tool install php@8.4")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Keep the current PHP" }));
    await waitFor(() => {
      expect(screen.queryByTestId("php-install-confirm")).not.toBeInTheDocument();
    });
    expect(screen.getByRole("combobox", { name: "PHP version" })).toHaveTextContent("8.3");
    expect(calls("tools.install")).toHaveLength(0);

    // Now confirm: install, then the switch is sent again.
    await pickPhp("8.4");
    const again = await screen.findByTestId("php-install-confirm");
    await user.click(within(again).getByRole("button", { name: "Install and switch" }));
    await waitFor(() => {
      expect(calls("tools.install")).toHaveLength(1);
    });
    expect(calls("tools.install")[0]?.params).toMatchObject({ specs: ["php@8.4"] });
    await waitFor(
      () => {
        expect(screen.getByText("PHP 8.4 is active")).toBeInTheDocument();
      },
      { timeout: 4000 },
    );
    // First attempt refused (not installed), the retry went through.
    expect(phpCalls().filter((call) => (call.params as { php: string }).php === "8.4").length).toBe(
      3,
    );
  });

  it("never sends a second switch while one is in flight", async () => {
    await open();
    await pickPhp("8.2");
    const combo = screen.getByRole("combobox", { name: "PHP version" });
    await userEvent.setup().click(combo);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByText("PHP 8.2 is active")).toBeInTheDocument();
    });
    expect(phpCalls()).toHaveLength(1);
  });
});

describe("Processos", () => {
  // `sites.ps` keeps answering the old state: only the events can move the row.
  const staleProcesses = (data: ScenarioData): FakeHandlers => {
    const frozen = structuredClone(must(data.sites[0]).processes);
    return { "sites.ps": () => ({ processes: structuredClone(frozen) }) };
  };

  async function openProcesses(extra?: (data: ScenarioData) => FakeHandlers) {
    const data = detailData();
    await open(data, extra?.(data));
    await userEvent.setup().click(await screen.findByRole("tab", { name: "Processes" }));
    return screen.findByRole("list", { name: "Processes" });
  }

  it("lists the processes and follows service events that carry the site id", async () => {
    const list = await openProcesses(staleProcesses);
    const web = must(list.querySelector('[data-process-row="web"]'));
    const queue = must(list.querySelector('[data-process-row="queue"]'));
    expect(web).toHaveAttribute("data-state", "running");
    expect(queue).toHaveAttribute("data-state", "stopped");
    await act(async () => {
      harness?.fake.publish("service.stopped", {
        v: 1,
        instance_id: "01JPROC00000000000000000WEB",
        name: "loja.test/web",
        tool: "procfile",
        state: "stopped",
        pid: null,
        strategy: null,
        exit_code: 0,
        error: null,
        site_id: SITE_ID,
      });
      harness?.fake.flush();
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await waitFor(() => {
      expect(list.querySelector('[data-process-row="web"]')).toHaveAttribute(
        "data-state",
        "stopped",
      );
    });
  });

  it("starts a process through sites.run and the row follows the events", async () => {
    const list = await openProcesses();
    const queue = must(list.querySelector<HTMLElement>('[data-process-row="queue"]'));
    await userEvent.setup().click(within(queue).getByRole("button", { name: "Start" }));
    await waitFor(() => {
      expect(calls("sites.run")[0]?.params).toEqual({
        site: "loja.test",
        process: "queue",
        action: "start",
      });
    });
    await waitFor(() => {
      expect(list.querySelector('[data-process-row="queue"]')).toHaveAttribute(
        "data-state",
        "running",
      );
    });
  });

  it("opens the process log in the shared LogViewer (app:<host>/<process>)", async () => {
    const list = await openProcesses();
    const queue = must(list.querySelector<HTMLElement>('[data-process-row="queue"]'));
    const user = userEvent.setup();
    const button = within(queue).getByRole("button", { name: "Logs: queue" });
    expect(button).toBeEnabled();
    await user.click(button);
    const viewer = await within(queue).findByTestId("log-viewer");
    expect(within(viewer).getByRole("log", { name: "Log lines of loja.test/queue" })).toBeVisible();
    expect(viewer.querySelector("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge logs app:loja.test/queue -f",
    );
    await user.click(within(queue).getByRole("button", { name: "Logs: queue" }));
    expect(within(queue).queryByTestId("log-viewer")).toBeNull();
  });
});

describe("Diagnóstico", () => {
  it("renders ok, warning and failed checks with copyable fixes and tests DNS", async () => {
    await open();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("tab", { name: "Diagnostics" }));
    const list = await screen.findByRole("list", { name: "Diagnostics" });
    await waitFor(() => {
      expect(list.querySelector('[data-check="site.dns"]')).toHaveAttribute("data-status", "ok");
    });
    expect(list.querySelector('[data-check="site.root_access"]')).toHaveAttribute(
      "data-status",
      "warn",
    );
    const failed = must(list.querySelector<HTMLElement>('[data-check="site.php_pool"]'));
    expect(failed).toHaveAttribute("data-status", "fail");
    expect(within(failed).getByTestId("check-fix")).toHaveTextContent("forge start php");

    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    await user.click(within(failed).getByRole("button", { name: "Copy fix" }));
    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith("forge start php");
    });

    await user.click(screen.getByRole("button", { name: "Test DNS" }));
    expect(await within(screen.getByTestId("dns-test")).findByRole("status")).toHaveTextContent(
      "loja.test resolves to 127.0.0.1 (via forgebox-dns).",
    );
    expect(calls("net.dns_test")[0]?.params).toEqual({ host: "loja.test" });
  });
});

describe("tab slots", () => {
  it("renders a tab registered by another epic next to the built-in ones", async () => {
    const unregister = registerSiteTab({
      id: "performance",
      order: 20,
      label: { ns: "sites", key: "detail.tabs.general" },
      component: ({ siteId }) => <p data-testid="perf-tab">perf of {siteId}</p>,
    });
    try {
      await open();
      const tabs = await screen.findAllByRole("tab");
      // General (10), performance (20), processes (30), env (40), database (50), diagnostics (90).
      expect(tabs.map((tab) => tab.textContent)).toEqual([
        "General",
        "General",
        "Processes",
        "Environment",
        "Database",
        "Diagnostics",
      ]);
      await userEvent.setup().click(must(tabs[1]));
      expect(await screen.findByTestId("perf-tab")).toHaveTextContent(SITE_ID);
    } finally {
      unregister();
    }
  });
});
