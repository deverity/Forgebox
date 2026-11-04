// Pure model of the Dashboard "Projetos" section (docs/14 section 4.2): the 24 h ranking and the
// idle-site detection. Nothing here touches React or the daemon.
import { renderCli } from "../../ipc/cli";
import type * as T from "../../ipc/generated";

export type ProjectSortKey = "cpu" | "mem" | "p99" | "errors";
export type SortDirection = "asc" | "desc";

export const SORT_KEYS: readonly ProjectSortKey[] = ["cpu", "mem", "p99", "errors"];
/** A site without a request for longer than this, with processes running, is "idle". */
export const IDLE_DAYS = 7;
const DAY_MS = 24 * 60 * 60 * 1000;

function valueOf(site: T.SiteStatsDto, key: ProjectSortKey): number | null {
  const value = {
    cpu: site.cpu_pct,
    mem: site.mem_bytes,
    p99: site.p99_ms,
    errors: site.error_pct,
  }[key];
  return value === null || Number.isNaN(value) ? null : value;
}

/**
 * The ranking: by `key` (highest first unless `direction` is "asc"). Sites without a value for
 * the key always go last, whatever the direction; ties break by host so the order is stable.
 */
export function sortProjects(
  sites: readonly T.SiteStatsDto[],
  key: ProjectSortKey,
  direction: SortDirection = "desc",
): T.SiteStatsDto[] {
  const sign = direction === "desc" ? -1 : 1;
  return [...sites].sort((a, b) => {
    const left = valueOf(a, key);
    const right = valueOf(b, key);
    if (left === null && right === null) return a.host.localeCompare(b.host);
    if (left === null) return 1;
    if (right === null) return -1;
    return sign * (left - right) || a.host.localeCompare(b.host);
  });
}

/** The value the sparkline of a row follows, for the active sort key. */
export function seriesValue(point: T.SeriesPointDto, key: ProjectSortKey): number | null {
  return {
    cpu: point.cpu_pct,
    mem: point.mem_bytes,
    p99: point.p99_ms,
    errors: point.error_pct,
  }[key];
}

export interface IdleSite {
  site: T.SiteStatsDto;
  /** Whole days since the last request; `null` when none was recorded within the retention. */
  idleDays: number | null;
  /** `forge run <host> web stop`. */
  stopCli: string | null;
}

/**
 * Sites idle for more than {@link IDLE_DAYS} days that still have processes running (the only
 * ones a stop frees anything for). No recorded request at all counts as idle: the daemon keeps
 * `last_request_at` for the whole retention, which is longer than 7 days.
 */
export function idleSites(sites: readonly T.SiteStatsDto[], now: number): IdleSite[] {
  const found: IdleSite[] = [];
  for (const site of sites) {
    if ((site.processes ?? 0) <= 0) continue;
    const at = site.last_request_at == null ? Number.NaN : Date.parse(site.last_request_at);
    const known = !Number.isNaN(at);
    if (known && now - at <= IDLE_DAYS * DAY_MS) continue;
    found.push({
      site,
      idleDays: known ? Math.floor((now - at) / DAY_MS) : null,
      stopCli: renderCli("sites.run", { site: site.host, process: "web", action: "stop" }),
    });
  }
  // Longest idle first; never-seen first of all.
  return found.sort(
    (a, b) =>
      (b.idleDays ?? Number.POSITIVE_INFINITY) - (a.idleDays ?? Number.POSITIVE_INFINITY) ||
      a.site.host.localeCompare(b.site.host),
  );
}
