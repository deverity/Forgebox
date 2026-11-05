// Pure helpers of the site Performance tab (M2-E5-T16): range options, the `stats.series`
// answer as the columnar series of the charts, event markers, and the tab's state machine.
// No React here, so the rules are unit-tested without a daemon.
import type * as T from "../../../ipc/generated";
import type {
  ChartMarker,
  LatencySeries,
  MarkerKind,
  ResourceSeries,
  ThroughputSeries,
} from "../charts/types";

/** Ranges of the control: 60 s is the live stream (`1m`, 2 s step), the rest are `stats.series`. */
export const RANGES = ["1m", "1h", "24h", "7d"] as const satisfies readonly T.StatsWindow[];
export type PerformanceRange = (typeof RANGES)[number];
export const DEFAULT_RANGE: PerformanceRange = "1m";

/** Rows shown by each table; the table height is fixed by it so skeleton and data match. */
export const TABLE_ROWS = 8;

export interface SeriesModel {
  latency: LatencySeries;
  throughput: ThroughputSeries;
  resource: ResourceSeries;
  markers: ChartMarker[];
  /** At least one request in the span (otherwise the charts show their empty state). */
  hasTraffic: boolean;
}

/** `stats.series` to the charts' columns (`t` in unix seconds, gaps as `null`). */
export function toSeriesModel(dto: T.SeriesDto): SeriesModel {
  const stepS = Math.max(dto.step_ms, 1) / 1000;
  const t: number[] = [];
  const p50: (number | null)[] = [];
  const p90: (number | null)[] = [];
  const p99: (number | null)[] = [];
  const rps: (number | null)[] = [];
  const e4xx: (number | null)[] = [];
  const e5xx: (number | null)[] = [];
  const cpu: (number | null)[] = [];
  const ram: (number | null)[] = [];
  let hasTraffic = false;
  for (const point of dto.points) {
    const at = Date.parse(point.at);
    if (Number.isNaN(at)) continue;
    t.push(at / 1000);
    p50.push(point.p50_ms);
    p90.push(point.p90_ms);
    p99.push(point.p99_ms);
    rps.push(point.rps ?? point.requests / stepS);
    e4xx.push(point.status_4xx / stepS);
    e5xx.push(point.status_5xx / stepS);
    cpu.push(point.cpu_pct);
    ram.push(point.mem_bytes);
    if (point.requests > 0) hasTraffic = true;
  }
  return {
    latency: { t, p50, p90, p99 },
    throughput: { t, rps, e4xx, e5xx },
    resource: { t, cpu, ram },
    markers: toMarkers(dto.markers),
    hasTraffic,
  };
}

/** Event kind of the daemon to the marker families of docs/14 section 4.1. */
export function markerKind(kind: string): MarkerKind | null {
  switch (kind) {
    case "site.php_changed":
      return "php-version";
    case "service.running":
    case "service.stopped":
    case "service.crashed":
      return "restart";
    case "config.rendered":
    case "site.updated":
      return "config";
    case "tool.updated":
    case "tool.rollback":
      return "update";
    default:
      return null;
  }
}

export function toMarkers(markers: readonly T.SeriesMarkerDto[]): ChartMarker[] {
  const out: ChartMarker[] = [];
  for (const marker of markers) {
    const kind = markerKind(marker.kind);
    const at = Date.parse(marker.at);
    if (kind === null || Number.isNaN(at)) continue;
    const { from, to } = marker.args;
    const detail =
      from !== undefined && to !== undefined
        ? `${from} → ${to}`
        : (marker.args.tool ?? marker.args.instance ?? marker.args.fs_path);
    out.push({
      id: `m${String(marker.seq)}`,
      t: at / 1000,
      kind,
      ...(detail === undefined ? {} : { detail }),
    });
  }
  return out;
}

/** Slowest routes first (p99, then mean); routes without latency are left out. */
export function slowestRoutes(routes: readonly T.RouteStatsDto[], limit = TABLE_ROWS) {
  return routes
    .filter((route) => route.p99_ms !== null || route.mean_ms !== null)
    .sort((a, b) => (b.p99_ms ?? b.mean_ms ?? 0) - (a.p99_ms ?? a.mean_ms ?? 0))
    .slice(0, limit);
}

/** Whether the site has a PHP pool (the only kind `sites.pool_mode` accepts). */
export function isPhpSite(site: T.SiteDto): boolean {
  return site.kind === "php";
}

export type PerformanceState = "unavailable" | "global-off" | "site-off" | "ready";

/** What the tab shows instead of (or around) the panels. Order matters: global before site. */
export function performanceState(input: {
  available: boolean;
  globalEnabled: boolean | null;
  site: T.SiteDto;
}): PerformanceState {
  if (!input.available) return "unavailable";
  if (input.globalEnabled === false) return "global-off";
  if (input.site.metrics_enabled === false) return "site-off";
  return "ready";
}
