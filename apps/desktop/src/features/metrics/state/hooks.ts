// React side of the metrics data layer: the per-site store hook and the TanStack Query hooks
// (stats.get / stats.series / stats.routes / stats.top). No paths and no context assumptions:
// everything goes through the active context's `ForgeApi` (works local or remote).
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useCallback, useMemo, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import type { ResultOf } from "../../../ipc/api";
import type { ForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";
import { forgeQuery, useCapability, useForgeApi, useRuntime } from "../../../state";
import { browserPageActivity } from "../../../state/page";
import { createMetricsFormatters, type MetricsFormatters } from "../format";
import { hubFor, type MetricsHub } from "./hub";
import { EMPTY_METRICS, type SiteMetricsState } from "./types";

/** `stats.top` refresh period while the window is visible. */
export const TOP_POLL_MS = 5000;
/** Default window of the live panels. */
export const DEFAULT_WINDOW: T.StatsWindow = "5m";

/** The metrics hub of the active context (one per runtime). */
export function useMetricsHub(): MetricsHub {
  const runtime = useRuntime();
  return hubFor(runtime, () => ({
    api: runtime.api,
    connection: runtime.stores.connection,
  }));
}

/**
 * Live metrics of one site over `window`, fed by `metrics.subscribe`. Samples are batched per
 * animation frame; consumers of the same (site, window) share one subscription, which closes
 * when the last one unmounts. A reconnect replaces the points with the daemon's new snapshot.
 * `siteId` null (or a daemon without `metrics.subscribe`) gives the idle state.
 */
export function useSiteMetrics(
  siteId: string | null,
  window: T.StatsWindow = DEFAULT_WINDOW,
): SiteMetricsState {
  const hub = useMetricsHub();
  const capability = useCapability("metrics.subscribe");
  const enabled = siteId !== null && capability.available;
  // React subscribes in its effect phase: that is where the reference on the shared store is
  // taken (and released), so a render that never commits never opens a stream.
  const subscribe = useCallback(
    (listener: () => void) => {
      if (!enabled) return () => undefined;
      const { store, release } = hub.acquire(siteId, window);
      const off = store.subscribe(listener);
      listener(); // the store may have changed between the render and this effect
      return () => {
        off();
        release();
      };
    },
    [hub, enabled, siteId, window],
  );
  const getSnapshot = useCallback(
    () => (enabled ? hub.peek(siteId, window) : undefined) ?? EMPTY_METRICS,
    [hub, enabled, siteId, window],
  );
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** `stats.get` for one site and window. */
export function useSiteStats(
  site: string | null,
  window: T.StatsWindow = DEFAULT_WINDOW,
): UseQueryResult<ResultOf<"stats.get">, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("stats.get");
  return useQuery({
    ...forgeQuery(api, "stats.get", { site, window }),
    enabled: site !== null && capability.available,
  });
}

/** `stats.series` (chart points and event markers) for one site. */
export function useSiteSeries(
  site: string | null,
  range: T.StatsWindow = "1h",
): UseQueryResult<ResultOf<"stats.series">, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("stats.series");
  return useQuery({
    ...forgeQuery(api, "stats.series", { site: site ?? "", range }),
    enabled: site !== null && capability.available,
  });
}

/** `stats.routes` (slowest routes and recent errors) for one site. */
export function useSiteRoutes(
  site: string | null,
): UseQueryResult<ResultOf<"stats.routes">, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("stats.routes");
  return useQuery({
    ...forgeQuery(api, "stats.routes", { site: site ?? "" }),
    enabled: site !== null && capability.available,
  });
}

/** Whether the document is visible (WebView2 reports a minimized window as hidden). */
export function usePageVisible(): boolean {
  const [page] = useState(() => browserPageActivity());
  return useSyncExternalStore(
    (listener) => page.subscribe(listener),
    () => page.visible(),
  );
}

/** `stats.top`, refreshed every 5 s only while the window is visible. */
export function useTop(
  options: { enabled?: boolean; intervalMs?: number } = {},
): UseQueryResult<ResultOf<"stats.top">, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("stats.top");
  const visible = usePageVisible();
  return useQuery({
    ...forgeQuery(api, "stats.top", {}),
    enabled: (options.enabled ?? true) && capability.available,
    refetchInterval: visible ? (options.intervalMs ?? TOP_POLL_MS) : false,
    refetchIntervalInBackground: false,
  });
}

/** Formatters for the active UI language (re-created when it switches). */
export function useMetricsFormatters(): MetricsFormatters {
  const { i18n } = useTranslation();
  return useMemo(() => createMetricsFormatters(i18n.language), [i18n.language]);
}
