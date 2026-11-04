// Metrics actions (M2-E5-T14, docs/07 section 1.7, docs/14 section 4). Registered here so the
// ESLint rule `forgebox/no-direct-mutation` and the parity gate see them, and so the screen
// shows the `forge` command rendered from the same params.
import type { ParamsOf } from "../ipc/api";
import type { SitePoolMode, StatsWindow } from "../ipc/generated";
import { renderCli } from "../ipc/cli";
import { defineAction } from "./registry";

/** Shared or dedicated PHP pool of one site (`forge site pool loja.test dedicated`). */
export const setSitePoolMode = defineAction<
  "sites.pool_mode",
  { site: string; mode: SitePoolMode }
>({
  id: "sites.set_pool_mode",
  method: "sites.pool_mode",
  toParams: ({ site, mode }) => ({ site, mode }),
});

/** Per-site metrics switch (`forge site metrics loja.test off`). */
export const setSiteMetrics = defineAction<"sites.metrics", { site: string; enabled: boolean }>({
  id: "sites.set_metrics",
  method: "sites.metrics",
  toParams: ({ site, enabled }) => ({ site, enabled }),
});

/** `forge stats <site> [--window w]`: the command behind the live panel. */
export function statsCli(site: string, window?: StatsWindow): string | null {
  const params: ParamsOf<"stats.get"> = window === undefined ? { site } : { site, window };
  return renderCli("stats.get", params);
}

/** `forge stats <site> --series --window w`. */
export function statsSeriesCli(site: string, range: StatsWindow): string | null {
  return renderCli("stats.series", { site, range });
}

/** `forge stats <site> --routes`. */
export function statsRoutesCli(site: string): string | null {
  return renderCli("stats.routes", { site });
}

/** `forge top`. */
export function topCli(): string | null {
  return renderCli("stats.top", {});
}

/** `forge stats <site> --window w --watch`. */
export function statsWatchCli(site: string, window: StatsWindow): string | null {
  return renderCli("metrics.subscribe", { site, window });
}
