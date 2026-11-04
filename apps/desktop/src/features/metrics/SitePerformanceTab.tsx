// "Desempenho" tab of the site detail (M2-E5-T16, docs/14 section 4.1). Registered through the
// site-tab slot registry (`slots.ts`); it knows its site only by id. Everything it reads goes
// through the active context's API (`stats.get` / `stats.series` / `stats.routes` and the
// `metrics.subscribe` stream of the 60 s mode), so it works the same against a remote daemon.
//
// Layout contract (docs/07 section 11, no layout shift): every region has a fixed height in its
// skeleton, empty and data states (KPI cells, chart boxes, the 8-row tables), so data arriving
// moves nothing.
import { Suspense, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { setConfig } from "../../actions/system";
import { setSiteMetrics, setSitePoolMode } from "../../actions/metrics";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Badge } from "../../design/primitives/Badge";
import { CliCommand } from "../../design/primitives/CliCommand";
import { EmptyState } from "../../design/primitives/EmptyState";
import { Skeleton } from "../../design/primitives/Skeleton";
import { renderCli } from "../../ipc/cli";
import type { SitePoolMode } from "../../ipc/generated";
import { useCapability, useConnectionStatus, useForgeQuery, useHello, useSite } from "../../state";
import { DEFAULT_CHART_HEIGHT } from "./charts/ChartFrame";
import { LatencyBandChart, ResourceChart, ThroughputErrorsChart } from "./charts";
import { ActionToggle } from "./components/ActionToggle";
import { KpiStrip } from "./components/KpiStrip";
import {
  DEFAULT_RANGE,
  isPhpSite,
  performanceState,
  toSeriesModel,
  type PerformanceRange,
} from "./components/model";
import { RangeControl } from "./components/RangeControl";
import { RoutesTables } from "./components/RoutesTables";
import {
  useMetricsFormatters,
  useSiteMetrics,
  useSiteRoutes,
  useSiteSeries,
  useSiteStats,
} from "./state";

/** Figure = caption (20) + gap (8) + canvas slot; fixed so the lazy chart replaces a same-size box. */
const CHART_BOX = DEFAULT_CHART_HEIGHT + 28;
/** KPIs and tables refresh this often while the tab is visible. */
const REFRESH_MS = 5000;
/** Test ids of the components below (props, so not literal JSX attributes). */
const IDS = {
  unavailable: "perf-state-unavailable",
  globalOff: "perf-state-global-off",
  siteOff: "perf-state-site-off",
  pool: "perf-pool-toggle",
  collect: "perf-collect-toggle",
} as const;
const GLOBAL_KEY = "metrics.enabled";

function ChartBox({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ height: CHART_BOX }} className="overflow-hidden">
      <Suspense fallback={<Skeleton height={CHART_BOX} />}>{children}</Suspense>
    </div>
  );
}

/** Notice of the states that replace the panels. */
function StateNotice({
  "data-testid": testId,
  title,
  description,
  action,
  command,
}: {
  "data-testid": string;
  title: string;
  description: string;
  action?: React.ReactNode;
  command?: string | null;
}) {
  return (
    <div data-testid={testId} className="flex flex-col items-center gap-3">
      <EmptyState title={title} description={description} action={action} />
      {command === undefined ? null : (
        <div className="w-full max-w-md">
          <CliCommand command={command} />
        </div>
      )}
    </div>
  );
}

export function SitePerformanceTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("metrics");
  const site = useSite(siteId);
  const hello = useHello();
  const status = useConnectionStatus();
  const formatters = useMetricsFormatters();
  const [range, setRange] = useState<PerformanceRange>(DEFAULT_RANGE);
  const live = range === "1m";

  const seriesCapability = useCapability("stats.series");
  const globalQuery = useForgeQuery("system.config_get", { key: GLOBAL_KEY });
  const globalValue = globalQuery.data?.value;
  const globalEnabled = typeof globalValue === "boolean" ? globalValue : null;
  const state =
    site === undefined
      ? "ready"
      : performanceState({
          // A dropped connection is not "unsupported": the last data stays up (reconnecting badge).
          available: seriesCapability.available || seriesCapability.reason === "disconnected",
          globalEnabled,
          site,
        });
  const active = state === "ready" && site !== undefined;
  const ref = active ? siteId : null;

  const now = useSiteStats(ref, "1m", { refetchMs: REFRESH_MS });
  const recent = useSiteStats(ref, "5m", { refetchMs: REFRESH_MS });
  const series = useSiteSeries(ref, range);
  const routes = useSiteRoutes(ref);
  // The 60 s mode follows `metrics.subscribe` (window 1m); it opens when the mode is entered
  // and closes when it is left or the tab unmounts (the hub counts references).
  const stream = useSiteMetrics(active && live ? siteId : null, "1m");
  const latestAt = stream.latest?.at;
  const seen = useRef<string | undefined>(undefined);
  const refetchSeries = series.refetch;
  useEffect(() => {
    // Every new live sample redraws the 60 s chart from the 2 s steps of `stats.series`, so the
    // points keep one meaning (a rolling-window sample is not a step). The first sample only
    // arms the comparison: the query just loaded is as fresh as that sample.
    if (!live) {
      seen.current = undefined;
      return;
    }
    if (latestAt === undefined || latestAt === seen.current) return;
    const first = seen.current === undefined;
    seen.current = latestAt;
    if (!first) void refetchSeries();
  }, [live, latestAt, refetchSeries]);

  if (site === undefined) return null;

  const php = isPhpSite(site);
  const remote = hello !== null && !hello.local;
  const reconnecting = status !== "connected";
  const poolShared = (site.pool_mode ?? "shared") === "shared";

  if (state === "unavailable") {
    return (
      <StateNotice
        data-testid={IDS.unavailable}
        title={t("site.states.unavailable.title")}
        description={t("site.states.unavailable.description")}
      />
    );
  }
  if (state === "global-off") {
    const input = { key: GLOBAL_KEY, value: true };
    return (
      <StateNotice
        data-testid={IDS.globalOff}
        title={t("site.states.globalOff.title")}
        description={t("site.states.globalOff.description")}
        action={
          <ActionButton action={setConfig} input={input} variant="primary">
            {t("site.states.globalOff.action")}
          </ActionButton>
        }
        command={renderCli("system.config_set", input)}
      />
    );
  }
  if (state === "site-off") {
    const input = { site: site.host, enabled: true };
    return (
      <StateNotice
        data-testid={IDS.siteOff}
        title={t("site.states.siteOff.title")}
        description={t("site.states.siteOff.description")}
        action={
          <ActionButton action={setSiteMetrics} input={input} variant="primary">
            {t("site.states.siteOff.action")}
          </ActionButton>
        }
        command={renderCli("sites.metrics", input)}
      />
    );
  }

  const model = series.data === undefined ? null : toSeriesModel(series.data);
  const traffic = model?.hasTraffic === true;
  const drawn = model !== null && traffic ? model : null;
  const resource =
    model !== null && (traffic || model.resource.cpu.some((v) => v !== null))
      ? model.resource
      : null;
  const chartsLoading = series.isPending;
  const kpiLoading = recent.isPending || now.isPending;
  const sample = (query: typeof now) =>
    query.data?.sites.find((s) => s.site_id === siteId) ?? query.data?.sites[0] ?? null;
  const markers = model?.markers ?? [];

  return (
    <div data-testid="site-performance" data-range={range} className="flex flex-col gap-4">
      <div className="flex min-h-8 flex-wrap items-center gap-3">
        <RangeControl value={range} onChange={setRange} />
        <div className="ml-auto flex items-center gap-2" data-testid="perf-badges">
          {reconnecting ? (
            <span role="status" data-testid="perf-reconnecting">
              <Badge tone="warning">{t("site.badge.reconnecting")}</Badge>
            </span>
          ) : live && stream.status === "live" ? (
            <Badge tone="success">{t("site.badge.live")}</Badge>
          ) : null}
          {remote ? (
            <span data-testid="perf-remote" title={t("site.remoteNote")}>
              <Badge tone="info">{t("site.badge.remote")}</Badge>
            </span>
          ) : null}
        </div>
      </div>
      {reconnecting ? (
        <p data-testid="perf-reconnecting-note" className="text-callout text-fg-secondary">
          {t("site.reconnectingNote")}
        </p>
      ) : null}

      <KpiStrip
        now={sample(now)}
        recent={sample(recent)}
        loading={kpiLoading}
        formatters={formatters}
        php={php}
      />

      {!chartsLoading && !traffic ? (
        <p data-testid="perf-no-traffic" className="text-callout text-fg-secondary">
          {t("site.noTraffic")}
        </p>
      ) : null}

      <div className="flex flex-col gap-4" data-testid="perf-charts">
        <ChartBox>
          <LatencyBandChart
            series={drawn?.latency ?? null}
            markers={markers}
            loading={chartsLoading}
          />
        </ChartBox>
        <ChartBox>
          <ThroughputErrorsChart
            series={drawn?.throughput ?? null}
            markers={markers}
            loading={chartsLoading}
          />
        </ChartBox>
        {php ? (
          <ChartBox>
            <ResourceChart series={resource} markers={markers} loading={chartsLoading} />
          </ChartBox>
        ) : (
          <p data-testid="perf-no-resource" className="text-callout text-fg-secondary">
            {t("site.noResource", { kind: t(`site.kind.${site.kind}`) })}
          </p>
        )}
      </div>

      <RoutesTables
        data={routes.data}
        loading={routes.isPending}
        failed={routes.isError}
        formatters={formatters}
      />

      <section aria-labelledby="perf-controls-title" className="flex flex-col gap-2">
        <h3 id="perf-controls-title" className="text-headline text-fg-primary">
          {t("site.controls.title")}
        </h3>
        <div className="grid gap-3 lg:grid-cols-2">
          <ActionToggle
            data-testid={IDS.pool}
            label={t("site.controls.pool.label")}
            description={t("site.controls.pool.hint")}
            action={setSitePoolMode}
            checked={!poolShared}
            inputFor={(next): { site: string; mode: SitePoolMode } => ({
              site: site.host,
              mode: next ? "dedicated" : "shared",
            })}
            {...(php
              ? {}
              : {
                  disabledReason: t("site.controls.pool.nonPhp", {
                    kind: t(`site.kind.${site.kind}`),
                  }),
                })}
          />
          <ActionToggle
            data-testid={IDS.collect}
            label={t("site.controls.collect.label")}
            description={t("site.controls.collect.hint")}
            action={setSiteMetrics}
            checked={site.metrics_enabled ?? true}
            inputFor={(next) => ({ site: site.host, enabled: next })}
          />
        </div>
      </section>
    </div>
  );
}
