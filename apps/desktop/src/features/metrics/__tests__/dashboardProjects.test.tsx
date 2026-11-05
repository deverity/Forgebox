// Acceptance M2-E5-T17 (screen): the ranking sorts, shows "≈" for apportioned values, is
// virtualized with lazy sparklines, re-renders only the row whose numbers changed, stops polling
// when hidden, links to the Desempenho tab; idle sites and performance alerts show their commands.
import { act, renderHook, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { projectsDashboard } from "../../../../qa/fixtures/scenarios/dashboard";
import { metricsHandlers } from "../../../../qa/fixtures/scenarios/metrics";
import { renderApp, type AppHarness } from "../../../app/testApp";
import { fakeError } from "../../../ipc/fakeBridge";
import type * as T from "../../../ipc/generated";
import { ForgeStateProvider } from "../../../state";
import { startHarness, populatedData, type Harness } from "../../../state/testFixtures";
import { mockReducedMotion } from "../../../test/matchMedia";
import { useAllSiteStats } from "../state/hooks";

// uPlot needs matchMedia at import time and jsdom draws nothing: the sparkline is a stub here.
vi.mock("../charts", () => ({ ChartSparkline: () => <i data-spark-stub="" /> }));

let app: AppHarness | null = null;
let hook: Harness | null = null;

beforeEach(() => {
  // jsdom has no layout: the virtualizer and the scroll areas read these.
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => 1000,
  });
});
afterEach(async () => {
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
  await app?.layer.dispose();
  await hook?.layer.dispose();
  app = null;
  hook = null;
});

async function open(patch: (rows: T.SiteStatsDto[]) => T.SiteStatsDto[] = (rows) => rows) {
  const { data: qa, extra } = projectsDashboard();
  const data = populatedData();
  data.sites = qa.sites;
  const base = metricsHandlers(qa).handlers;
  const handlers = {
    ...extra.handlers,
    "stats.get": (params: T.StatsParams, ctx: never) => {
      const result = (extra.handlers?.["stats.get"] as (p: T.StatsParams, c: never) => T.StatsDto)(
        params,
        ctx,
      );
      return { ...result, sites: patch(result.sites) };
    },
    "stats.series": base["stats.series"],
  };
  app = await renderApp({ path: "/dashboard", data, extra: { handlers } as never });
  await waitFor(() => {
    expect(document.querySelector("[data-project-row]")).not.toBeNull();
  });
  return app;
}

const rows = () => [...document.querySelectorAll<HTMLElement>("[data-project-row]")];
const section = () => screen.getByTestId("dashboard-projects");

describe("Projetos ranking", () => {
  it("keeps the shared-element transition by default", async () => {
    await open();
    expect(rows()[0]?.getAttribute("data-shared-transition")).toBe("true");
  });

  it("drops the shared-element transition under prefers-reduced-motion", async () => {
    mockReducedMotion(true);
    await open();
    const row = rows()[0];
    expect(row?.getAttribute("data-shared-transition")).toBe("false");
    expect(
      row?.querySelector<HTMLElement>("span")?.style.getPropertyValue("view-transition-name"),
    ).toBe("");
  });

  it("lists a window of the 200 sites, sorted by CPU, with ≈ on apportioned values", async () => {
    await open();
    expect(rows().length).toBeGreaterThan(3);
    expect(rows().length).toBeLessThan(40);
    expect(
      within(section()).getByRole("list", { name: /ranked over the last 24 hours/ }),
    ).toBeTruthy();
    const first = rows()[0];
    expect(first?.textContent).toContain("≈");
    // Highest CPU first: each row's CPU is not above the previous one.
    const cpu = rows().map((row) =>
      Number.parseFloat(row.children[1]?.textContent.replace(/[^\d.]/g, "") ?? "NaN"),
    );
    expect(cpu).toEqual([...cpu].sort((a, b) => b - a));
    expect(within(section()).getByRole("button", { name: "Sort by CPU" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("re-sorts by p99 and by errors when the header is clicked", async () => {
    await open();
    const user = userEvent.setup();
    await user.click(within(section()).getByRole("button", { name: "Sort by p99" }));
    await waitFor(() => {
      expect(rows()[0]).toHaveTextContent("site1.test");
    });
    await user.click(within(section()).getByRole("button", { name: "Sort by Errors" }));
    await waitFor(() => {
      expect(rows()[0]).toHaveTextContent("site2.test");
    });
  });

  it("shows no ≈ when the values are exact", async () => {
    await open((all) => all.map((site) => ({ ...site, approx: false })));
    expect(rows()[0]?.textContent).not.toContain("≈");
  });

  it("fetches sparklines only for the mounted rows", async () => {
    const h = await open();
    await waitFor(() => {
      expect(h.fake.calls.some((call) => call.method === "stats.series")).toBe(true);
    });
    const series = h.fake.calls.filter((call) => call.method === "stats.series").length;
    expect(series).toBeLessThanOrEqual(rows().length);
    expect(series).toBeLessThan(40);
  });

  it("re-renders only the row whose numbers changed", async () => {
    vi.stubEnv("MODE", "qa");
    let target = "";
    let bump = 0;
    const h = await open((all) =>
      all.map((site) =>
        site.site_id === target ? { ...site, cpu_pct: (site.cpu_pct ?? 0) + bump } : site,
      ),
    );
    await waitFor(() => {
      expect(rows().length).toBeGreaterThan(5);
    });
    // A mounted row (the third of the ranking) gets a new sample; its order does not change.
    target = rows()[2]?.dataset.projectRow ?? "";
    expect(target).not.toBe("");
    window.__FORGEBOX_RENDERS__?.reset();
    bump = 0.001;
    await act(async () => {
      await h.layer.queryClient.refetchQueries({
        predicate: (query) => JSON.stringify(query.queryKey).includes('"stats.get"'),
      });
    });
    await waitFor(() => {
      expect(window.__FORGEBOX_RENDERS__?.renders()[`project:${target}`]?.total).toBe(1);
    });
    const counted = Object.keys(window.__FORGEBOX_RENDERS__?.renders() ?? {}).filter((key) =>
      key.startsWith("project:"),
    );
    expect(counted).toEqual([`project:${target}`]);
  });

  it("opens the Desempenho tab of the site on click", async () => {
    const h = await open();
    const row = rows()[0];
    expect(row).toBeDefined();
    if (row === undefined) return;
    await userEvent.setup().click(row);
    await waitFor(() => {
      expect(h.router.state.location.pathname).toBe(`/sites/${row.dataset.projectRow ?? ""}`);
    });
    expect(h.router.state.location.search).toMatchObject({ tab: "performance" });
  });

  it("shows the CLI hints of the ranking and the machine monitor", async () => {
    await open();
    const hints = [...document.querySelectorAll("[data-forge-cli]")].map((node) =>
      node.getAttribute("data-forge-cli"),
    );
    expect(hints).toContain("forge stats --window 24h");
    expect(hints).toContain("forge top");
  });
});

describe("idle sites", () => {
  it("lists the 8 day site with its stop command and leaves the 6 day one out", async () => {
    await open();
    const idle = await screen.findByTestId("projects-idle");
    expect(within(idle).getByText("site3.test")).toBeInTheDocument();
    expect(within(idle).queryByText("site4.test")).toBeNull();
    expect(idle.querySelector("[data-forge-cli]")).toHaveAttribute(
      "data-forge-cli",
      "forge run site3.test web stop",
    );
  });
});

describe("Machine monitor", () => {
  it("shows CPU, RAM and disk against the Forgebox share", async () => {
    await open();
    const monitor = await screen.findByTestId("dashboard-machine");
    await waitFor(() => {
      expect(within(monitor).getByTestId("machine-cpu")).toBeInTheDocument();
    });
    expect(within(monitor).getByTestId("machine-ram")).toBeInTheDocument();
    expect(within(monitor).getByTestId("machine-disk-root")).toBeInTheDocument();
    expect(
      within(monitor).getByTestId("machine-cpu").querySelector("[data-forgebox-share]"),
    ).not.toBeNull();
  });

  it("R-15: shows the cause and a retry when stats.get fails, never an endless skeleton", async () => {
    const { data: qa, extra } = projectsDashboard();
    const data = populatedData();
    data.sites = qa.sites;
    const handlers = {
      ...extra.handlers,
      "stats.get": () => fakeError("internal", "stats.get failed", "fake_error"),
    };
    app = await renderApp({ path: "/dashboard", data, extra: { handlers } });
    const projects = await screen.findByTestId("dashboard-projects");
    await waitFor(
      () => {
        expect(projects.querySelector('[data-async-view="error"]')).not.toBeNull();
      },
      { timeout: 4000 },
    );
    expect(projects.querySelector("[data-skeleton]")).toBeNull();
    expect(within(projects).getByRole("alert")).toBeInTheDocument();
    expect(within(projects).getByRole("button", { name: /try again|retry/i })).toBeInTheDocument();
  });

  it("shows the cause and a retry when stats.top fails, never an endless loading", async () => {
    const { data: qa, extra } = projectsDashboard();
    const data = populatedData();
    data.sites = qa.sites;
    let fail = true;
    const handlers = {
      ...extra.handlers,
      "stats.top": (params: unknown, ctx: unknown) => {
        if (fail) fakeError("internal", "stats.top failed", "fake_error");
        return (extra.handlers?.["stats.top"] as (p: unknown, c: unknown) => unknown)(params, ctx);
      },
    };
    app = await renderApp({ path: "/dashboard", data, extra: { handlers } as never });
    const monitor = await screen.findByTestId("dashboard-machine");
    await waitFor(() => {
      expect(monitor.querySelector('[data-async-view="error"]')).not.toBeNull();
    });
    expect(monitor.querySelector("[aria-busy='true']")).toBeNull();
    expect(within(monitor).getByRole("alert")).toBeInTheDocument();
    fail = false;
    await userEvent
      .setup()
      .click(within(monitor).getByRole("button", { name: /try again|retry/i }));
    await waitFor(() => {
      expect(within(monitor).getByTestId("machine-cpu")).toBeInTheDocument();
    });
  });

  it("shows the disconnected state with the daemon away", async () => {
    app = await renderApp({ scenario: "daemon-down", path: "/dashboard" });
    const monitor = await screen.findByTestId("dashboard-machine");
    await waitFor(() => {
      expect(monitor.querySelector('[data-async-view="disconnected"]')).not.toBeNull();
    });
    expect(monitor.querySelector("[aria-busy='true']")).toBeNull();
  });
});

describe("Atenção performance alerts", () => {
  it("raises p99, errors, pool RAM and disk once each, with commands", async () => {
    await open();
    await waitFor(() => {
      expect(document.querySelectorAll('[data-attention-item^="performance/"]')).toHaveLength(4);
    });
    const ids = [...document.querySelectorAll('[data-attention-item^="performance/"]')].map(
      (node) => node.getAttribute("data-attention-item"),
    );
    expect(ids.sort()).toEqual([
      expect.stringContaining("disk:root"),
      expect.stringContaining("errors:"),
      expect.stringContaining("p99:"),
      expect.stringContaining("pool-ram:8.3"),
    ]);
    for (const node of document.querySelectorAll('[data-attention-item^="performance/"]')) {
      expect(node.querySelector("[data-forge-cli]")).not.toBeNull();
    }
  });
});

describe("polling", () => {
  it("stops while the page is hidden", async () => {
    hook = await startHarness({
      scenario: {
        handlers: { "stats.get": () => ({ window: "24h", sites: [] }) },
      },
    });
    const h = hook;
    const wrapper = ({ children }: { children: ReactNode }) => (
      <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
    );
    renderHook(() => useAllSiteStats("24h", { intervalMs: 40 }), { wrapper });
    await waitFor(() => {
      expect(h.count("stats.get")).toBeGreaterThanOrEqual(3);
    });
    const hidden = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    const frozen = h.count("stats.get");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 200));
    });
    expect(h.count("stats.get")).toBe(frozen);
    hidden.mockRestore();
  });
});
