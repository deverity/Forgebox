// Performance alerts of the Dashboard Atenção (docs/14 section 5, M2-E5-T17). Client-side rules
// over what the daemon already answers: nothing is sent anywhere and native toasts stay
// deferred (ADR-0044 section 13). Each rule yields at most one item per site, pool or volume,
// with cause, action and the equivalent `forge` command.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AttentionItem } from "../../app/slots";
import { renderCli } from "../../ipc/cli";
import type * as T from "../../ipc/generated";
import { usePools } from "../../state";
import { useAllSiteStats, useTop } from "./state/hooks";

export const METRICS_NS = "metrics";
/** p99 above this over the {@link ALERT_WINDOW} window raises the latency alert. */
export const P99_LIMIT_MS = 800;
/** Error rate (percent of requests) above this raises the error alert. */
export const ERROR_LIMIT_PCT = 5;
/** Resident memory of one PHP pool above this (bytes). */
export const POOL_RAM_LIMIT = 1024 ** 3;
/** Free space of a volume below this (bytes). */
export const DISK_FREE_LIMIT = 2 * 1024 ** 3;
/**
 * "p99 > 800 ms for 5 min" is read as the p99 of the whole 5 min window (`stats.get` 5m): a
 * percentile over the window cannot exceed the limit unless that much of the traffic did.
 */
export const ALERT_WINDOW: T.StatsWindow = "5m";
/**
 * Fewer requests than this in the window is noise (one slow request is a p99 of 100 %), so
 * the traffic rules stay quiet. Documented in docs/14 section 5 with this task.
 */
export const MIN_REQUESTS = 10;

const NO_ITEMS: readonly AttentionItem[] = [];

function cliOf(method: "stats.get" | "stats.top", params: object): { cli?: string } {
  const command = renderCli(method, params);
  return command === null ? {} : { cli: command };
}

/** Latency and error-rate alerts of the sites, from `stats.get` over {@link ALERT_WINDOW}. */
export function siteTrafficItems(sites: readonly T.SiteStatsDto[]): AttentionItem[] {
  const items: AttentionItem[] = [];
  for (const site of sites) {
    if (site.requests < MIN_REQUESTS) continue;
    const href = `/sites/${site.site_id}`;
    const search = { tab: "performance" };
    const cli = cliOf("stats.get", { site: site.host, window: ALERT_WINDOW });
    if (site.p99_ms !== null && site.p99_ms > P99_LIMIT_MS) {
      items.push({
        id: `p99:${site.site_id}`,
        severity: "warning",
        ns: METRICS_NS,
        title: { key: "attention.p99.title", values: { host: site.host } },
        cause: {
          key: "attention.p99.cause",
          values: { value: Math.round(site.p99_ms), limit: P99_LIMIT_MS, minutes: 5 },
        },
        ...cli,
        href,
        search,
        actionLabel: { key: "attention.action.performance", values: {} },
      });
    }
    if (site.error_pct !== null && site.error_pct > ERROR_LIMIT_PCT) {
      items.push({
        id: `errors:${site.site_id}`,
        severity: "critical",
        ns: METRICS_NS,
        title: { key: "attention.errors.title", values: { host: site.host } },
        cause: {
          key: "attention.errors.cause",
          values: { value: Math.round(site.error_pct * 10) / 10, limit: ERROR_LIMIT_PCT },
        },
        ...cli,
        href,
        search,
        actionLabel: { key: "attention.action.performance", values: {} },
      });
    }
  }
  return items;
}

/** One alert per PHP pool whose workers hold more than {@link POOL_RAM_LIMIT}. */
export function poolRamItems(pools: readonly T.PoolDto[]): AttentionItem[] {
  return pools
    .filter((pool) => (pool.rss_bytes ?? 0) > POOL_RAM_LIMIT)
    .map((pool) => ({
      id: `pool-ram:${pool.series}`,
      severity: "warning" as const,
      ns: METRICS_NS,
      title: { key: "attention.poolRam.title", values: { series: pool.series } },
      cause: {
        key: "attention.poolRam.cause",
        values: { gb: Math.round(((pool.rss_bytes ?? 0) / 1024 ** 3) * 10) / 10, limit: 1 },
      },
      ...cliOf("stats.top", {}),
      href: "/php",
      actionLabel: { key: "attention.action.php", values: {} },
    }));
}

/** One alert per volume with less than {@link DISK_FREE_LIMIT} left for the daemon's user. */
export function diskItems(machine: T.MachineStatsDto | null | undefined): AttentionItem[] {
  if (machine === null || machine === undefined) return [];
  return machine.disks
    .filter((disk) => disk.available < DISK_FREE_LIMIT)
    .map((disk) => ({
      id: `disk:${disk.role}`,
      severity: "critical" as const,
      ns: METRICS_NS,
      title: { key: `attention.disk.title.${disk.role}`, values: {} },
      cause: {
        key: "attention.disk.cause",
        values: { gb: Math.round((disk.available / 1024 ** 3) * 10) / 10, limit: 2 },
      },
      ...cliOf("stats.top", {}),
      href: "/settings",
      actionLabel: { key: "attention.action.settings", values: {} },
    }));
}

/**
 * The texts of these items live in the lazy `metrics` namespace, which the Atenção list does not
 * load (its own are in `dashboard`): the source holds its items back until it is there, so the
 * list never shows raw keys. Never suspends.
 */
function useMetricsTexts(): boolean {
  const { i18n } = useTranslation();
  const [ready, setReady] = useState(() => i18n.hasLoadedNamespace(METRICS_NS));
  useEffect(() => {
    if (ready) return undefined;
    let live = true;
    void i18n.loadNamespaces(METRICS_NS).then(() => {
      if (live) setReady(true);
    });
    return () => {
      live = false;
    };
  }, [i18n, ready]);
  return ready;
}

/**
 * The Atenção source. `stats.top` is the machine monitor's own query (one call for both);
 * `stats.get` 5m is polled like the ranking's, only while the page is visible.
 */
export function usePerformanceItems(): readonly AttentionItem[] {
  const stats = useAllSiteStats(ALERT_WINDOW);
  const top = useTop();
  const pools = usePools();
  const texts = useMetricsTexts();
  const sites = stats.data?.sites;
  const machine = top.data?.machine;
  return useMemo(() => {
    if (!texts) return NO_ITEMS;
    const all = [
      ...siteTrafficItems(sites ?? []),
      ...poolRamItems(Object.values(pools)),
      ...diskItems(machine),
    ];
    return all.length === 0 ? NO_ITEMS : all;
  }, [texts, sites, machine, pools]);
}
