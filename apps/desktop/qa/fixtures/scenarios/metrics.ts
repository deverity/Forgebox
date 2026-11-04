// Handlers of the site Performance tab (M2-E5-T16): `stats.get|series|routes`, `sites.metrics`,
// `sites.pool_mode` and the global `metrics.enabled` key over the QA data, with deterministic
// numbers (fixed seed, relative to the clock so the charts always end "now").
//
// Variants through the URL (`?metrics=`): `global-off`, `site-off` (every site), `no-traffic`,
// `pool-fails` (sites.pool_mode ends in job.failed), `slow-pool` (the job takes 1.2 s).
//
// The same module answers the Dashboard "Projetos" ranking and machine monitor (M2-E5-T17):
// `stats.get` of all sites (the 24h window varies per site so a ranking has an order),
// `stats.top` with the machine, and `StatsOptions` overrides that trip the docs/14 section 5
// rules in the `dashboard-projects` scenario. Without overrides every rule stays quiet.
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import type { QaData } from "./data";

type Variant = "global-off" | "site-off" | "no-traffic" | "pool-fails" | "slow-pool" | null;

function variant(): Variant {
  if (typeof window === "undefined") return null;
  const value = new URLSearchParams(window.location.search).get("metrics");
  return value === "global-off" ||
    value === "site-off" ||
    value === "no-traffic" ||
    value === "pool-fails" ||
    value === "slow-pool"
    ? value
    : null;
}

function rng(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const STEP_MS: Record<T.StatsWindow, number> = {
  "1m": 2000,
  "5m": 2000,
  "1h": 10_000,
  "24h": 60_000,
  "7d": 3_600_000,
};
const SPAN_MS: Record<T.StatsWindow, number> = {
  "1m": 60_000,
  "5m": 300_000,
  "1h": 3_600_000,
  "24h": 86_400_000,
  "7d": 604_800_000,
};

function findSite(data: QaData, ref: string): T.SiteDto {
  const found = data.sites.find((s) => s.id === ref || s.host === ref);
  if (found === undefined) fakeError("not_found", `site ${ref} not found`);
  return found;
}

function series(site: T.SiteDto, range: T.StatsWindow, quiet: boolean): T.SeriesDto {
  const step = STEP_MS[range];
  const count = Math.round(SPAN_MS[range] / step);
  const end = Math.ceil(Date.now() / step) * step;
  const rand = rng(11);
  const php = site.kind === "php";
  const points: T.SeriesPointDto[] = [];
  for (let i = 0; i < count; i += 1) {
    const at = end - (count - i) * step;
    const wave = Math.sin(i / (count / 6));
    const rate = quiet ? 0 : Math.max(0, 14 + 9 * wave + rand() * 4);
    const requests = Math.round((rate * step) / 1000);
    const p50 = 28 + 10 * wave + rand() * 5;
    const p90 = p50 * (1.8 + rand() * 0.3);
    const p99 = p90 * (1.5 + rand() * 0.5);
    points.push({
      at: new Date(at).toISOString(),
      requests,
      rps: quiet ? null : rate,
      bytes: requests * 4096,
      status_4xx: quiet ? 0 : Math.round(requests * 0.03 * rand()),
      status_5xx: quiet ? 0 : i % 61 < 3 ? Math.round(requests * 0.2) : 0,
      error_pct: null,
      p50_ms: requests === 0 ? null : p50,
      p90_ms: requests === 0 ? null : p90,
      p99_ms: requests === 0 ? null : p99,
      cpu_pct: php ? 18 + 10 * wave + rand() * 4 : null,
      mem_bytes: php ? (190 + 30 * wave + rand() * 4) * 1024 * 1024 : null,
      workers_active: php ? 2 + Math.round(wave + 1) : null,
      approx: (site.pool_mode ?? "shared") === "shared",
      sampled: false,
    });
  }
  const at = (back: number) => new Date(end - Math.floor(count * back) * step).toISOString();
  return {
    site_id: site.id,
    host: site.host,
    range,
    step_ms: step,
    from: new Date(end - count * step).toISOString(),
    to: new Date(end).toISOString(),
    points,
    markers: quiet
      ? []
      : [
          { seq: 101, at: at(0.7), kind: "site.php_changed", args: { from: "8.3", to: "8.4" } },
          { seq: 102, at: at(0.45), kind: "service.running", args: { tool: "nginx" } },
          { seq: 103, at: at(0.2), kind: "config.rendered", args: { owner: "nginx" } },
        ],
  };
}

const MIB = 1024 ** 2;
const GIB = 1024 ** 3;

export interface StatsOptions {
  /** Per-host overrides of the `stats.get` entry (every window). */
  sites?: Readonly<Record<string, Partial<T.SiteStatsDto>>>;
  machine?: Partial<T.MachineStatsDto>;
}

export function healthyMachine(
  patch: Partial<T.MachineStatsDto> = {},
  now = Date.now(),
): T.MachineStatsDto {
  return {
    at: new Date(now).toISOString(),
    cpu_pct: 22,
    logical_cpus: 8,
    mem_total: 16 * GIB,
    mem_available: 9 * GIB,
    forgebox_cpu_pct: 4,
    forgebox_rss: 900 * MIB,
    disks: [{ role: "root", total: 500 * GIB, free: 220 * GIB, available: 220 * GIB }],
    ...patch,
  };
}

function stats(
  site: T.SiteDto,
  window: T.StatsWindow,
  quiet: boolean,
  index = 0,
  patch: Partial<T.SiteStatsDto> = {},
  now = Date.now(),
): T.SiteStatsDto {
  const php = site.kind === "php";
  const seconds = SPAN_MS[window] / 1000;
  const rate = quiet ? 0 : window === "1m" ? 16.4 : 14.2;
  // The 24h window is the Dashboard ranking: a little variation per site gives it an order
  // (all healthy: p99 < 420 ms, errors < 0.4 %, memory < 130 MiB). Other windows are the tab's.
  const ranked = window === "24h" && !quiet;
  return {
    site_id: site.id,
    host: site.host,
    window,
    at: new Date(now).toISOString(),
    requests: Math.round(rate * seconds),
    rps: quiet ? null : rate,
    p50_ms: quiet ? null : 31,
    p90_ms: quiet ? null : 58,
    p99_ms: quiet ? null : ranked ? 120 + ((index * 7) % 300) : 142,
    error_pct: quiet ? null : ranked ? (index % 4) / 10 : 1.8,
    cpu_pct: php ? (ranked ? 1 + (index % 13) / 2 : 21.5) : null,
    mem_bytes: php ? (ranked ? 40 + ((index * 3) % 90) : 214) * MIB : null,
    approx: (site.pool_mode ?? "shared") === "shared",
    sampled: false,
    workers_active: php ? 3 : null,
    metrics_enabled: site.metrics_enabled ?? true,
    processes: 1,
    last_request_at: quiet ? null : new Date(now - 60_000).toISOString(),
    ...patch,
  };
}

const ROUTES: [string, string, number, number, number, number][] = [
  ["POST", "/checkout/{id}", 1180, 640, 1420, 64],
  ["GET", "/api/orders", 910, 310, 780, 42],
  ["GET", "/products/{id}", 460, 120, 310, 57],
  ["GET", "/search", 380, 96, 260, 22],
  ["POST", "/login", 240, 88, 190, 12],
  ["GET", "/", 120, 22, 54, 530],
  ["GET", "/cart", 96, 40, 110, 71],
  ["GET", "/assets/{id}", 44, 8, 19, 902],
];

function routes(site: T.SiteDto, quiet: boolean): T.RoutesDto {
  const now = Date.now();
  const iso = (ago: number) => new Date(now - ago).toISOString();
  return {
    site_id: site.id,
    host: site.host,
    since: iso(18 * 3_600_000),
    at: iso(0),
    total: quiet ? 0 : 2400,
    routes: quiet
      ? []
      : ROUTES.map(([method, route, p99, p50, mean, count]) => ({
          method,
          route,
          count,
          count_min: count,
          observed: count,
          status_4xx: 0,
          status_5xx: route === "/api/orders" ? 3 : 0,
          error_pct: route === "/api/orders" ? 3 : 0,
          mean_ms: mean / 2,
          p50_ms: p50,
          p90_ms: p99 * 0.7,
          p99_ms: p99,
        })),
    recent_errors: quiet
      ? []
      : [
          [502, "GET", "/api/orders", 1210],
          [500, "POST", "/checkout/{id}", 840],
          [404, "GET", "/assets/{id}", 4],
          [404, "GET", "/favicon.ico", 2],
          [500, "POST", "/checkout/{id}", 910],
        ].map(([status, method, route, ms], index) => ({
          at: iso((index + 1) * 95_000),
          status: Number(status),
          method: String(method),
          route: String(route),
          host: site.host,
          duration_ms: Number(ms),
          upstream_ms: Number(ms) - 1,
          bytes: 0,
        })),
  };
}

export interface MetricsQa {
  handlers: FakeHandlers;
  /** `metrics.enabled` as the tab reads it. */
  globalEnabled(): boolean;
  setGlobalEnabled(value: boolean): void;
}

/** The Performance tab's handlers over `data` (they edit the site rows like the daemon). */
export function metricsHandlers(data: QaData, options: StatsOptions = {}): MetricsQa {
  const mode = variant();
  // One clock per scenario: a poll that changes nothing answers equal objects, so only the rows
  // whose numbers changed re-render.
  const now = Date.now();
  let globalEnabled = mode !== "global-off";
  if (mode === "site-off") for (const s of data.sites) s.metrics_enabled = false;
  const quiet = mode === "no-traffic";
  const patch = (site: T.SiteDto, change: Partial<T.SiteDto>): T.SiteDto => {
    const next = { ...site, ...change };
    data.sites.splice(data.sites.indexOf(site), 1, next);
    return next;
  };
  return {
    globalEnabled: () => globalEnabled,
    setGlobalEnabled: (value) => {
      globalEnabled = value;
    },
    handlers: {
      "stats.get": (params) => {
        const window = params.window ?? "5m";
        const ref = params.site;
        const sites =
          ref === undefined || ref === null
            ? data.sites
            : [findSite(data, typeof ref === "string" ? ref : "")];
        return {
          window,
          sites: sites.map((s) =>
            stats(s, window, quiet, data.sites.indexOf(s), options.sites?.[s.host], now),
          ),
        };
      },
      "stats.top": () => ({ processes: [], machine: healthyMachine(options.machine, now) }),
      "stats.series": (params) => series(findSite(data, params.site), params.range ?? "1h", quiet),
      "stats.routes": (params) => routes(findSite(data, params.site), quiet),
      "sites.metrics": (params, ctx) => {
        const site = findSite(data, params.site);
        const next = patch(site, { metrics_enabled: params.enabled });
        ctx.fake.publish("site.updated", { v: 1, site_id: next.id, host: next.host });
        return next;
      },
      "sites.pool_mode": (params, ctx) => {
        const site = findSite(data, params.site);
        if (site.kind !== "php") {
          fakeError("invalid_input", `${site.host} has no PHP pool`, "not_php_site");
        }
        if (mode === "pool-fails") {
          return fakeJob({
            steps: [{ pct: 30, message: "starting pool" }],
            stepMs: 300,
            outcome: { failed: { code: "internal", message: "pool did not start" } },
          });
        }
        const delay = mode === "slow-pool" ? 1200 : 300;
        const next: T.SiteDto = { ...site, pool_mode: params.mode };
        setTimeout(() => {
          patch(site, { pool_mode: params.mode });
          ctx.fake.publish("site.updated", { v: 1, site_id: site.id, host: site.host });
        }, delay);
        return fakeJob({
          steps: [{ pct: 50, message: "starting pool" }],
          stepMs: delay,
          outcome: { done: next },
        });
      },
    },
  };
}
