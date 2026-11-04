// Acceptance of M2-E2-T14: no Atenção when healthy, each of the 7 sources alone gives exactly one
// item with its command, sparklines fill after 30 polls and stop with the screen, an incoming
// tool.install.completed tops Atividade in both locales, and the tile strip keeps its height.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import i18next from "i18next";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../../App";
import { dashboardSections, registerDashboardSection } from "../../app/slots";
import { createTestRouter } from "../../app/router";
import { warmAppModules } from "../../app/testApp";
import { createFakeBridge, type FakeScenario } from "../../ipc/fakeBridge";
import type * as T from "../../ipc/generated";
import { StateLayer } from "../../state/layer";
import { manualPageActivity } from "../../state/page";
import {
  CA_STATUS,
  DNS_STATUS,
  IDS,
  populatedData,
  stateScenario,
  timerFrameClock,
  tool,
  type ScenarioData,
} from "../../state/testFixtures";

const HEALTHY: FakeScenario["handlers"] = {
  "services.ports": () => ({ ports: [], external: [] }),
  "tls.list": () => ({ certificates: [] }),
  "net.dns_status": () => ({ ...DNS_STATUS, enabled: true, policy: "applied", mode: "nrpt" }),
};

function data(): ScenarioData {
  const base = populatedData();
  base.tools = [tool("php", { name: "PHP", current: "8.3.36" }), ...base.tools];
  return base;
}

interface Mounted {
  fake: ReturnType<typeof createFakeBridge>;
  layer: StateLayer;
  page: ReturnType<typeof manualPageActivity>;
  router: ReturnType<typeof createTestRouter>;
  unmount: () => void;
}

async function mount(
  extra: FakeScenario = {},
  options: { data?: ScenarioData; metricsIntervalMs?: number; path?: string } = {},
): Promise<Mounted> {
  await warmAppModules();
  window.innerWidth = 1280;
  const fake = createFakeBridge(
    stateScenario(options.data ?? data(), {
      ...extra,
      handlers: { ...HEALTHY, ...extra.handlers },
    }),
  );
  const page = manualPageActivity();
  const layer = new StateLayer(fake, {
    page,
    frame: timerFrameClock(),
    rttIntervalMs: 0,
    snapshotDebounceMs: 0,
    invalidateDebounceMs: 0,
    ...(options.metricsIntervalMs === undefined
      ? {}
      : { metricsIntervalMs: options.metricsIntervalMs }),
  });
  const router = createTestRouter(options.path ?? "/dashboard");
  const view = render(<App layer={layer} router={router} />);
  if ((options.path ?? "/dashboard") === "/dashboard") {
    await screen.findByTestId("page-dashboard");
  }
  await act(async () => {
    await layer.runtime("local").settled();
  });
  return { fake, layer, page, router, unmount: view.unmount };
}

async function publish(m: Mounted, kind: string, payload: unknown) {
  await act(async () => {
    m.fake.publish(kind, payload);
    m.fake.flush();
    await m.layer.runtime("local").settled();
  });
}

const items = () => [...document.querySelectorAll<HTMLElement>("[data-attention-item]")];

afterEach(async () => {
  await i18next.changeLanguage("en-US");
});

describe("Atenção", () => {
  it("is not in the DOM on a healthy fixture", async () => {
    await mount();
    await screen.findByText("mysql 1.0.0");
    // Let the queries (ports, certificates, drift history) answer.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });
    expect(screen.queryByTestId("dashboard-attention")).toBeNull();
    expect(items()).toHaveLength(0);
  });

  const DAY = 24 * 60 * 60 * 1000;
  const inDays = (days: number) => new Date(Date.now() + days * DAY).toISOString();

  interface Case {
    name: string;
    extra?: FakeScenario;
    publish?: [string, unknown];
    cli: string | RegExp;
    text: string;
  }
  const CASES: Case[] = [
    {
      name: "external port conflict (services.ports)",
      extra: {
        handlers: {
          "services.ports": () => ({
            ports: [],
            external: [{ port: 3306, protocol: "tcp", holder: { pid: 4, exe: "mysqld.exe" } }],
          }),
        },
      },
      cli: "forge service ports",
      text: "Port 3306 is in use by mysqld.exe",
    },
    {
      name: "port.conflict event",
      publish: [
        "port.conflict",
        {
          v: 1,
          port: 6379,
          protocol: "tcp",
          owner_name: "redis/default",
          instance_id: null,
          holder: { pid: 7, exe: "redis-server.exe" },
        },
      ],
      cli: "forge service ports",
      text: "Port 6379 is in use by redis-server.exe",
    },
    {
      name: "available updates",
      publish: [
        "tool.updates_available",
        { v: 1, count: 1, updates: [{ tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" }] },
      ],
      cli: "forge tool update --check",
      text: "1 tool update available",
    },
    {
      name: "available update known from tools.list",
      extra: {
        handlers: {
          "tools.list": () => ({
            tools: [tool("php", { name: "PHP", current: "8.3.36", update_available: "8.3.37" })],
          }),
        },
      },
      cli: "forge tool update --check",
      text: "1 tool update available",
    },
    {
      name: "config drift",
      publish: [
        "config.drift_detected",
        { v: 1, owner: "nginx", path: "C:/Forgebox/etc/nginx.conf", fs_path: null },
      ],
      cli: "forge doctor",
      text: "Config file changed outside Forgebox",
    },
    {
      name: "CA not trusted",
      extra: {
        handlers: {
          "tls.ca_status": () => ({
            ...CA_STATUS,
            trusted_current_user: false,
            trusted_local_machine: false,
          }),
        },
      },
      cli: /^forge tls ca install/,
      text: "The local CA is not trusted",
    },
    {
      name: "CA expiring",
      extra: { handlers: { "tls.ca_status": () => ({ ...CA_STATUS, not_after: inDays(20) }) } },
      cli: "forge tls ca rotate",
      text: "The local CA expires in",
    },
    {
      name: "certificate expiring",
      extra: {
        handlers: {
          "tls.list": () => ({
            certificates: [
              {
                host: "loja.test",
                sans: ["loja.test"],
                site_id: IDS.site,
                not_before: inDays(-60),
                not_after: inDays(10),
                thumbprint_sha256: "AA",
              },
            ],
          }),
        },
      },
      cli: "forge tls renew --all",
      text: "1 certificate expires in less than 30 days",
    },
    {
      name: "DNS in hosts fallback",
      extra: {
        handlers: {
          "net.dns_status": () => ({
            ...DNS_STATUS,
            enabled: true,
            policy: "hosts_fallback",
            mode: "hosts",
            reason: "resolver_probe_failed",
          }),
        },
      },
      cli: "forge dns status",
      text: "DNS is using the hosts file fallback",
    },
    {
      name: "failed service",
      publish: [
        "service.failed",
        {
          v: 1,
          instance_id: IDS.redis,
          name: "redis/default",
          tool: "redis",
          state: "failed",
          pid: null,
          strategy: null,
          exit_code: 1,
          error: { code: "exit", message: "exited" },
        },
      ],
      cli: "forge restart redis/default",
      text: "redis/default failed",
    },
  ];

  it.each(CASES)("$name alone gives exactly one item with its command", async (c) => {
    const m = await mount(c.extra);
    if (c.publish !== undefined) await publish(m, c.publish[0], c.publish[1]);
    await waitFor(() => {
      expect(items()).toHaveLength(1);
    });
    const [item] = items();
    expect(item).toHaveTextContent(c.text);
    const command = item?.querySelector("[data-forge-cli]")?.getAttribute("data-forge-cli") ?? "";
    if (typeof c.cli === "string") expect(command).toBe(c.cli);
    else expect(command).toMatch(c.cli);
    // Exactly one: nothing else in the section.
    expect(document.querySelectorAll("[data-forge-cli]").length).toBeGreaterThanOrEqual(1);
  });

  it("a config.rendered after the drift removes the item again", async () => {
    const m = await mount();
    const payload = { v: 1, owner: "nginx", path: "C:/Forgebox/etc/nginx.conf", fs_path: null };
    await publish(m, "config.drift_detected", payload);
    await waitFor(() => {
      expect(items()).toHaveLength(1);
    });
    await publish(m, "config.rendered", payload);
    await waitFor(() => {
      expect(items()).toHaveLength(0);
    });
  });
});

describe("Projetos", () => {
  it("shows an empty state, not an error, while nothing registers the section", async () => {
    // The metrics feature registers it on load: take it out for this test.
    const saved = dashboardSections.list();
    dashboardSections.clear();
    try {
      await mount();
      const section = await screen.findByTestId("dashboard-projects");
      expect(within(section).getByText("Project ranking is not available yet")).toBeInTheDocument();
      expect(within(section).queryByRole("alert")).toBeNull();
    } finally {
      for (const entry of saved) registerDashboardSection(entry);
    }
  });

  it("renders the registered section instead", async () => {
    const off = registerDashboardSection({
      id: "projects",
      label: { ns: "dashboard", key: "projects.title" },
      component: () => <p>ranking from E5</p>,
    });
    try {
      await mount();
      expect(await screen.findByText("ranking from E5")).toBeInTheDocument();
      expect(screen.queryByTestId("dashboard-projects")).toBeNull();
    } finally {
      off();
    }
  });
});

describe("tiles", () => {
  it("shows one tile per instance and pool with state, port and the actions", async () => {
    await mount();
    const mysql = await screen.findByText("mysql 1.0.0");
    const tile = mysql.closest("[data-tile]");
    expect(tile).not.toBeNull();
    if (tile === null) return;
    expect(tile).toHaveAttribute("data-target", "mysql/default");
    expect(within(tile as HTMLElement).getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(
      within(tile as HTMLElement).getByRole("button", { name: "More actions for mysql/default" }),
    ).toBeInTheDocument();
    expect(document.querySelectorAll("[data-tile]")).toHaveLength(3);
    expect(document.querySelector('[data-target="php@8.3"]')).not.toBeNull();
  });

  it("keeps the strip height from skeleton to data", async () => {
    const slow: FakeScenario = {
      handlers: {
        "services.status": async () => {
          await new Promise((resolve) => setTimeout(resolve, 600));
          return { instances: data().instances, pools: [] };
        },
      },
    };
    await warmAppModules();
    const fake = createFakeBridge(
      stateScenario(data(), { ...slow, handlers: { ...HEALTHY, ...slow.handlers } }),
    );
    const layer = new StateLayer(fake, {
      page: manualPageActivity(),
      frame: timerFrameClock(),
      rttIntervalMs: 0,
      snapshotDebounceMs: 0,
      invalidateDebounceMs: 0,
    });
    render(<App layer={layer} router={createTestRouter("/dashboard")} />);
    const loading = await waitFor(() => {
      const el = document.querySelector<HTMLElement>(
        '[data-testid="dashboard-tiles"] [data-async-view=loading]',
      );
      if (el === null) throw new Error("no skeleton yet");
      return el;
    });
    const skeletonHeight = loading.style.height;
    const ready = await waitFor(() => {
      const el = document.querySelector<HTMLElement>(
        '[data-testid="dashboard-tiles"] [data-async-view=ready]',
      );
      if (el === null) throw new Error("not ready");
      return el;
    });
    expect(skeletonHeight).not.toBe("");
    expect(ready.style.height).toBe(skeletonHeight);
  });
});

describe("sparklines and polling", () => {
  it("fill after 30 polls", async () => {
    const m = await mount({}, { metricsIntervalMs: 5 });
    const points = (): number => {
      const polyline = document.querySelector('[data-target="mysql/default"] svg polyline');
      return (polyline?.getAttribute("points") ?? "").split(" ").filter(Boolean).length;
    };
    await waitFor(
      () => {
        expect(
          m.fake.calls.filter((call) => call.method === "services.status").length,
        ).toBeGreaterThanOrEqual(31);
        expect(points()).toBe(30);
      },
      { timeout: 10_000 },
    );
  });

  it("navigating away releases every Dashboard consumer; hidden window then makes 0 calls", async () => {
    const m = await mount({}, { metricsIntervalMs: 20, path: "/dashboard" });
    const poller = m.layer.runtime("local").metrics;
    const calls = () => m.fake.calls.filter((call) => call.method === "services.status").length;
    await waitFor(() => {
      expect(calls()).toBeGreaterThan(2);
    });
    const onDashboard = poller.consumerCount();
    await act(async () => {
      await m.router.navigate({ to: "/settings" });
    });
    await screen.findByTestId("page-settings");
    const onSettings = poller.consumerCount();
    // The status bar stays a consumer, so the Dashboard's own consumers must be gone.
    expect(onSettings).toBeGreaterThan(0);
    expect(onDashboard).toBeGreaterThan(onSettings);
    // Same count as a fresh mount straight on /settings.
    m.unmount();
    const fresh = await mount({}, { metricsIntervalMs: 20, path: "/settings" });
    await screen.findByTestId("page-settings");
    expect(fresh.layer.runtime("local").metrics.consumerCount()).toBe(onSettings);
    // Window hidden: nothing polls, even with the status bar mounted.
    await act(async () => {
      fresh.page.set({ visible: false });
      await new Promise((resolve) => setTimeout(resolve, 60));
    });
    const before = fresh.fake.calls.filter((call) => call.method === "services.status").length;
    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(fresh.fake.calls.filter((call) => call.method === "services.status").length).toBe(
      before,
    );
  });

  it("stop with the screen: no services.status calls afterwards", async () => {
    const m = await mount({}, { metricsIntervalMs: 20, path: "/dashboard" });
    await waitFor(() => {
      expect(
        m.fake.calls.filter((call) => call.method === "services.status").length,
      ).toBeGreaterThan(2);
    });
    // Leave the shell too: its status bar is the other consumer of the same poll.
    m.unmount();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    const before = m.fake.calls.filter((call) => call.method === "services.status").length;
    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(m.fake.calls.filter((call) => call.method === "services.status").length).toBe(before);
  });
});

describe("Atividade", () => {
  const installed = {
    v: 1,
    job_id: null,
    tool: "php",
    version: "8.3.37",
    version_id: null,
    progress: null,
    error: null,
  };

  it("puts an incoming tool.install.completed on top, in English", async () => {
    const m = await mount();
    const feed = await screen.findByTestId("dashboard-activity");
    await publish(m, "service.running", {
      v: 1,
      instance_id: IDS.mysql,
      name: "mysql/default",
      tool: "mysql",
      state: "running",
      pid: 1,
      strategy: null,
      exit_code: null,
      error: null,
    });
    await publish(m, "tool.install.completed", installed);
    await waitFor(() => {
      const first = feed.querySelector("li");
      expect(first).toHaveTextContent("PHP 8.3.37 installed");
      expect(first).toHaveTextContent("now");
    });
    expect(feed.querySelectorAll("li")).toHaveLength(2);
  });

  it("speaks Portuguese when the language is pt-BR", async () => {
    await i18next.changeLanguage("pt-BR");
    const m = await mount();
    const feed = await screen.findByTestId("dashboard-activity");
    await publish(m, "tool.install.completed", installed);
    await waitFor(() => {
      const first = feed.querySelector("li");
      expect(first).toHaveTextContent("PHP 8.3.37 instalado");
      expect(first).toHaveTextContent("agora");
    });
  });

  it("lists the history from events.list and keeps at most 50", async () => {
    const events = Array.from({ length: 60 }, (_, i) => ({
      kind: "site.created",
      payload: { v: 1, site_id: IDS.site, host: `s${String(i)}.test` },
    }));
    await mount({ events });
    const feed = await screen.findByTestId("dashboard-activity");
    await waitFor(() => {
      expect(feed.querySelectorAll("li")).toHaveLength(50);
    });
    expect(feed.querySelector("li")).toHaveTextContent("Site s59.test created");
  });
});

describe("Sites recentes", () => {
  it("shows the six most recent and an empty state without sites", async () => {
    const many = data();
    many.sites = Array.from({ length: 8 }, (_, i) => ({
      ...populatedData().sites[0],
      id: `01JSITE0000000000000000S${String(i).padStart(2, "0")}`,
      host: `s${String(i)}.test`,
      updated_at: new Date(Date.UTC(2026, 9, 1 + i)).toISOString(),
    })) as T.SiteDto[];
    await mount({}, { data: many });
    const recent = await screen.findByTestId("dashboard-recent");
    await waitFor(() => {
      expect(recent.querySelectorAll("[data-recent-site]")).toHaveLength(6);
    });
    expect(recent.querySelector("li")).toHaveTextContent("s7.test");
  });

  it("explains the empty list", async () => {
    const none = data();
    none.sites = [];
    await mount({}, { data: none });
    expect(await screen.findByText("No sites yet")).toBeInTheDocument();
  });
});
