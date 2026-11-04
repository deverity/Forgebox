// Acceptance M2-E5-T16: every state renders its pt-BR / en-US copy, the pool toggle goes
// loading -> success and loading -> error with the CLI, a range switch calls stats.series with
// the right range, the 60 s mode subscribes and unsubscribes, and the markers/series model.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { configHandlers } from "../../../../qa/fixtures/scenarios/config";
import { renderApp, type AppHarness } from "../../../app/testApp";
import { fakeError, fakeJob, type FakeHandlers } from "../../../ipc/fakeBridge";
import type * as T from "../../../ipc/generated";
import { populatedData, site as siteDto, type ScenarioData } from "../../../state/testFixtures";
import { markerKind, slowestRoutes, toMarkers, toSeriesModel } from "../components/model";

// jsdom has no canvas: uPlot is replaced by a recorder (the charts have their own suite).
vi.mock("uplot", () => {
  class Fake {
    over = document.createElement("div");
    scales = { x: { min: 0, max: 1 } };
    width = 600;
    height = 200;
    setData = vi.fn();
    setSize = vi.fn();
    destroy = vi.fn();
    constructor(public opts: { hooks?: { draw?: ((u: Fake) => void)[] } }) {
      for (const hook of opts.hooks?.draw ?? []) hook(this);
    }
    valToPos(): number {
      return 0;
    }
  }
  return { default: Fake };
});

const SITE_ID = "01JSITE00000000000000000PRF";
const STATIC_ID = "01JSITE00000000000000000STA";
let harness: AppHarness | null = null;
/** Open streams just before the tab opened (the shell has its own). */
let streamsBefore = 0;

beforeAll(async () => {
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 600 });
  await i18n.loadNamespaces(["metrics", "errors"]);
});

afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
  await i18n.changeLanguage("en-US");
  vi.restoreAllMocks();
});

const AT = "2026-11-04T12:00:00Z";

function stats(patch: Partial<T.SiteStatsDto> = {}): T.SiteStatsDto {
  return {
    site_id: SITE_ID,
    host: "loja.test",
    window: "5m",
    at: AT,
    requests: 600,
    rps: 2,
    p50_ms: 12,
    p90_ms: 40,
    p99_ms: 180,
    error_pct: 1.5,
    cpu_pct: 8,
    mem_bytes: 200 * 1024 * 1024,
    approx: true,
    sampled: false,
    workers_active: 3,
    ...patch,
  };
}

function series(requests = 5): T.SeriesDto {
  const points: T.SeriesPointDto[] = [0, 1, 2].map((i) => ({
    at: new Date(Date.parse(AT) + i * 10_000).toISOString(),
    requests,
    rps: requests / 10,
    bytes: 100,
    status_4xx: 1,
    status_5xx: 0,
    error_pct: 10,
    p50_ms: requests === 0 ? null : 10,
    p90_ms: requests === 0 ? null : 20,
    p99_ms: requests === 0 ? null : 30,
    cpu_pct: 5,
    mem_bytes: 1000,
    workers_active: 1,
    approx: true,
    sampled: false,
  }));
  return {
    site_id: SITE_ID,
    host: "loja.test",
    range: "1h",
    step_ms: 10_000,
    from: AT,
    to: AT,
    points,
    markers: [{ seq: 7, at: AT, kind: "site.php_changed", args: { from: "8.3", to: "8.4" } }],
  };
}

const routes: T.RoutesDto = {
  site_id: SITE_ID,
  host: "loja.test",
  since: AT,
  at: AT,
  total: 100,
  routes: [
    {
      method: "GET",
      route: "/fast",
      count: 50,
      count_min: 50,
      observed: 50,
      status_4xx: 0,
      status_5xx: 0,
      error_pct: 0,
      mean_ms: 5,
      p50_ms: 5,
      p90_ms: 6,
      p99_ms: 7,
    },
    {
      method: "POST",
      route: "/checkout/{id}",
      count: 10,
      count_min: 10,
      observed: 10,
      status_4xx: 0,
      status_5xx: 2,
      error_pct: 20,
      mean_ms: 900,
      p50_ms: 800,
      p90_ms: 1200,
      p99_ms: 1500,
    },
  ],
  recent_errors: [
    {
      at: AT,
      status: 502,
      method: "GET",
      route: "/api/orders",
      host: "loja.test",
      duration_ms: 30,
      upstream_ms: 29,
      bytes: 0,
    },
  ],
};

function data(): ScenarioData {
  const d = populatedData();
  d.sites = [
    siteDto(SITE_ID, "loja.test", { kind: "php", metrics_enabled: true, pool_mode: "shared" }),
    siteDto(STATIC_ID, "docs.test", { kind: "static", php: null, metrics_enabled: true }),
  ];
  return d;
}

interface Options {
  data?: ScenarioData;
  handlers?: FakeHandlers;
  site?: string;
  hello?: Partial<T.HelloDto>;
}

async function open(options: Options = {}): Promise<AppHarness> {
  const d = options.data ?? data();
  harness = await renderApp({
    path: `/sites/${options.site ?? SITE_ID}`,
    data: d,
    extra: {
      notImplemented: [],
      ...(options.hello === undefined ? {} : { hello: options.hello }),
      handlers: {
        ...configHandlers(d),
        "system.config_get": (params) => ({ key: params.key, value: true, is_default: true }),
        "stats.get": (params) => ({
          window: params.window ?? "5m",
          sites: [stats(params.window === "1m" ? { window: "1m", rps: 4.5 } : {})],
        }),
        "stats.series": (params) => ({ ...series(), range: params.range ?? "1h" }),
        "stats.routes": () => routes,
        ...options.handlers,
      },
    },
  });
  await screen.findByRole("heading", { level: 1 });
  streamsBefore = harness.fake.openStreams();
  await userEvent.setup().click(await screen.findByRole("tab", { name: "Performance" }));
  return harness;
}

const calls = (method: string) =>
  harness?.fake.calls.filter((call) => call.method === method) ?? [];

describe("states", () => {
  it("shows KPIs, tables and controls with data (en-US)", async () => {
    await open();
    const kpis = await screen.findByTestId("perf-kpis");
    await waitFor(() => {
      expect(within(kpis).getByText("4.5 req/s")).toBeInTheDocument();
    });
    expect(within(kpis).getByText("180 ms")).toBeInTheDocument();
    expect(within(kpis).getByText("≈ 8%")).toBeInTheDocument();
    expect(within(kpis).getByText("1.5%")).toBeInTheDocument();
    const slow = await screen.findByTestId("perf-routes");
    await waitFor(() => {
      expect(within(slow).getByText("/checkout/{id}")).toBeInTheDocument();
    });
    // Slowest first.
    const rows = within(slow).getAllByRole("row");
    expect(rows[1]).toHaveAttribute("data-route", "/checkout/{id}");
    const errors = screen.getByTestId("perf-errors");
    expect(await within(errors).findByText("502")).toBeInTheDocument();
    expect(screen.queryByText(/deep instrumentation|Instrumentação profunda/i)).toBeNull();
  });

  it("renders the same screen in pt-BR", async () => {
    await i18n.changeLanguage("pt-BR");
    await i18n.loadNamespaces("metrics");
    harness = null;
    await renderApp({
      path: `/sites/${SITE_ID}`,
      data: data(),
      extra: {
        notImplemented: [],
        handlers: {
          "system.config_get": (params) => ({ key: params.key, value: true, is_default: true }),
          "stats.get": () => ({ window: "5m", sites: [stats()] }),
          "stats.series": () => series(),
          "stats.routes": () => routes,
        },
      },
    }).then((h) => {
      harness = h;
    });
    await userEvent.setup().click(await screen.findByRole("tab", { name: "Desempenho" }));
    expect(await screen.findByText("Rotas mais lentas")).toBeInTheDocument();
    expect(screen.getByText("Erros recentes")).toBeInTheDocument();
    expect(screen.getByText("Pool dedicado")).toBeInTheDocument();
    expect(screen.getByText("Coletar métricas deste site")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "7 dias" })).toBeInTheDocument();
  });

  it("globally off: empty state with the action and the forge config command", async () => {
    await open({
      handlers: { "system.config_get": (p) => ({ key: p.key, value: false, is_default: false }) },
    });
    const state = await screen.findByTestId("perf-state-global-off");
    expect(within(state).getByText("Metrics are off")).toBeInTheDocument();
    expect(
      state.querySelector("[data-forge-cli='forge config set metrics.enabled true']"),
    ).not.toBeNull();
    const user = userEvent.setup();
    await user.click(within(state).getByRole("button", { name: "Turn metrics on" }));
    await waitFor(() => {
      expect(calls("system.config_set")[0]?.params).toEqual({
        key: "metrics.enabled",
        value: true,
      });
    });
  });

  it("site off: empty state that turns collection back on", async () => {
    const d = data();
    d.sites = [siteDto(SITE_ID, "loja.test", { kind: "php", metrics_enabled: false })];
    await open({ data: d });
    const state = await screen.findByTestId("perf-state-site-off");
    expect(
      state.querySelector("[data-forge-cli='forge site metrics loja.test on']"),
    ).not.toBeNull();
    await userEvent
      .setup()
      .click(within(state).getByRole("button", { name: "Collect metrics for this site" }));
    await waitFor(() => {
      expect(calls("sites.metrics")[0]?.params).toEqual({ site: "loja.test", enabled: true });
    });
  });

  it("no traffic: says so and keeps the charts' empty state", async () => {
    await open({ handlers: { "stats.series": () => series(0) } });
    expect(await screen.findByTestId("perf-no-traffic")).toHaveTextContent("No traffic");
    await waitFor(() => {
      expect(document.querySelector("[data-chart-state='empty']")).not.toBeNull();
    });
  });

  it("static site: no resource chart, no workers and the pool toggle explains why", async () => {
    await open({ site: STATIC_ID });
    expect(await screen.findByTestId("perf-no-resource")).toHaveTextContent("Static site");
    const pool = screen.getByTestId("perf-pool-toggle");
    expect(within(pool).getByRole("switch")).toBeDisabled();
    expect(within(pool).getByTestId("perf-pool-toggle-reason")).toHaveTextContent(
      "Only PHP sites have a pool",
    );
    expect(within(screen.getByTestId("perf-kpis")).getAllByText("—").length).toBeGreaterThan(0);
  });

  it("remote daemon: badge", async () => {
    await open({ hello: { local: false } });
    expect(await screen.findByTestId("perf-remote")).toHaveTextContent("Remote daemon");
  });

  it("reconnecting: badge and note while the data stays", async () => {
    const h = await open();
    await screen.findByTestId("perf-kpis");
    act(() => {
      h.fake.setStatus("reconnecting");
    });
    expect(await screen.findByTestId("perf-reconnecting")).toHaveTextContent("Reconnecting");
    expect(screen.getByTestId("perf-reconnecting-note")).toBeInTheDocument();
    expect(screen.getByTestId("perf-kpis")).toBeInTheDocument();
  });
});

describe("pool toggle", () => {
  it("loading -> success, with the CLI of the change", async () => {
    await open({
      handlers: {
        "sites.pool_mode": () =>
          fakeJob({
            steps: [{ pct: 50, message: "starting" }],
            stepMs: 50,
            outcome: { done: siteDto(SITE_ID, "loja.test", { pool_mode: "dedicated" }) },
          }),
      },
    });
    const pool = await screen.findByTestId("perf-pool-toggle");
    expect(pool).toHaveAttribute("data-forge-cli", "forge site pool loja.test dedicated");
    await userEvent.setup().click(within(pool).getByRole("switch"));
    expect(pool).toHaveAttribute("data-phase", "running");
    expect(within(pool).getByRole("switch")).toBeDisabled();
    expect(within(pool).getByText("Applying…")).toBeInTheDocument();
    await waitFor(() => {
      expect(pool).toHaveAttribute("data-phase", "done");
    });
    expect(within(pool).getByRole("switch")).toBeChecked();
    expect(calls("sites.pool_mode")[0]?.params).toEqual({ site: "loja.test", mode: "dedicated" });
  });

  it("loading -> error: back to shared, cause and the CLI stay", async () => {
    await open({
      handlers: {
        "sites.pool_mode": () => {
          fakeError("internal", "pool did not start");
        },
      },
    });
    const pool = await screen.findByTestId("perf-pool-toggle");
    await userEvent.setup().click(within(pool).getByRole("switch"));
    await waitFor(() => {
      expect(pool).toHaveAttribute("data-phase", "failed");
    });
    expect(within(pool).getByRole("switch")).not.toBeChecked();
    expect(within(pool).getByRole("alert")).toBeInTheDocument();
    expect(pool).toHaveAttribute("data-forge-cli", "forge site pool loja.test dedicated");
  });
});

describe("ranges", () => {
  it("opens in 60 s, switching calls stats.series with the range and leaves the stream", async () => {
    const h = await open();
    await screen.findByTestId("perf-kpis");
    await waitFor(() => {
      expect(calls("stats.series").at(-1)?.params).toEqual({ site: SITE_ID, range: "1m" });
    });
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(streamsBefore + 1);
    });
    const user = userEvent.setup();
    await user.click(screen.getByRole("radio", { name: "24 h" }));
    await waitFor(() => {
      expect(calls("stats.series").at(-1)?.params).toEqual({ site: SITE_ID, range: "24h" });
    });
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(streamsBefore);
    });
    await user.click(screen.getByRole("radio", { name: "7 days" }));
    await waitFor(() => {
      expect(calls("stats.series").at(-1)?.params).toEqual({ site: SITE_ID, range: "7d" });
    });
    await user.click(screen.getByRole("radio", { name: "1 h" }));
    await waitFor(() => {
      expect(calls("stats.series").at(-1)?.params).toEqual({ site: SITE_ID, range: "1h" });
    });
    await user.click(screen.getByRole("radio", { name: "60 s" }));
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(streamsBefore + 1);
    });
    await user.click(await screen.findByRole("tab", { name: "General" }));
    await waitFor(() => {
      expect(h.fake.openStreams()).toBe(streamsBefore);
    });
  });
});

describe("model", () => {
  it("maps series points to columns with gaps and rates", () => {
    const model = toSeriesModel(series());
    expect(model.hasTraffic).toBe(true);
    expect(model.latency.t).toHaveLength(3);
    expect(model.throughput.e4xx[0]).toBeCloseTo(0.1);
    expect(model.markers).toEqual([
      { id: "m7", t: Date.parse(AT) / 1000, kind: "php-version", detail: "8.3 → 8.4" },
    ]);
    expect(toSeriesModel(series(0)).hasTraffic).toBe(false);
    expect(toSeriesModel(series(0)).latency.p50[0]).toBeNull();
  });

  it("maps event kinds to marker families and skips the unknown", () => {
    expect(markerKind("service.crashed")).toBe("restart");
    expect(markerKind("config.rendered")).toBe("config");
    expect(markerKind("tool.rollback")).toBe("update");
    expect(markerKind("job.done")).toBeNull();
    expect(toMarkers([{ seq: 1, at: AT, kind: "job.done", args: {} }])).toEqual([]);
  });

  it("sorts the slowest routes by p99 and drops those without latency", () => {
    const base = routes.routes[0];
    if (base === undefined) throw new Error("fixture has routes");
    const list = slowestRoutes([
      { ...base, route: "/a", p99_ms: 10 },
      { ...base, route: "/b", p99_ms: 90 },
      { ...base, route: "/c", p99_ms: null, mean_ms: null },
    ]);
    expect(list.map((r) => r.route)).toEqual(["/b", "/a"]);
  });
});
