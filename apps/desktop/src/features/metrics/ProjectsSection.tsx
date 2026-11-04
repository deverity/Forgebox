// Dashboard "Projetos" (docs/14 section 4.2, docs/07 section 5.2): the 24 h ranking of the sites
// by CPU, RAM, p99 and errors, the sites idle for more than 7 days with a suggested stop, and
// the way into each site's Desempenho tab. Registered in the dashboard section slot by
// `slots.ts`. The list is virtualized (200+ sites), its sparklines load only for the rows that
// are mounted, and every row is memoized over the daemon's own (structurally shared) object, so
// a poll that changes one site re-renders one row.
import { Link } from "@tanstack/react-router";
import { Component, Suspense, memo, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { AsyncView } from "../../design/primitives/AsyncView";
import { Badge } from "../../design/primitives/Badge";
import { CliCommand } from "../../design/primitives/CliCommand";
import { useReducedMotion } from "../../design/useMotionPreset";
import { FOCUS_RING } from "../../design/primitives/shared";
import { VirtualList } from "../../design/primitives/VirtualList";
import { renderCli } from "../../ipc/cli";
import type * as T from "../../ipc/generated";
import { countRender } from "../../qa/render-counter";
import { useCapability, useConnectionStatus, useRuntime } from "../../state";
import { useNow } from "../services/useNow";
import { ChartSparkline } from "./charts";
import {
  IDLE_DAYS,
  SORT_KEYS,
  idleSites,
  seriesValue,
  sortProjects,
  type IdleSite,
  type ProjectSortKey,
  type SortDirection,
} from "./projects";
import { useAllSiteStats, useMetricsFormatters, useSiteSeries } from "./state";

/** Fixed row height: no measuring while scrolling. */
export const ROW_HEIGHT = 44;
/** Height of the ranking viewport (about 7 rows). */
const LIST_HEIGHT = 320;
const DETAIL_ROUTE = "/sites/$id";
/** Id of the Desempenho site tab (registered by M2-E5-T16). */
const PERFORMANCE_SEARCH = { tab: "performance" } as const;
const ARROWS = { desc: " ↓", asc: " ↑" } as const;
const SPARK_WIDTH = 120;
const SPARK_HEIGHT = 28;
const WINDOW: T.StatsWindow = "24h";
const IDLE_SHOWN = 5;
const CLOCK_MS = 60_000;
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const STATS_CONTEXT = { method: "stats.get", params: { window: WINDOW } } as const;
/** Same columns in the header and in every row. */
const COLUMNS = "grid-cols-[minmax(0,1fr)_88px_88px_80px_72px_120px]";

/** A chart that fails to load or draw leaves its cell empty, never the ranking. */
class ChartBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  override componentDidCatch(error: unknown) {
    console.error("project sparkline failed", error);
  }
  override render() {
    return this.state.failed ? null : this.props.children;
  }
}

function Sparkline({ siteId, sortKey }: { siteId: string; sortKey: ProjectSortKey }) {
  // Mounted only for the rows the virtualizer renders: that is the lazy load.
  const series = useSiteSeries(siteId, WINDOW);
  const points = series.data?.points;
  const data = useMemo(() => {
    if (points === undefined) return null;
    return {
      t: points.map((point) => Date.parse(point.at) / 1000),
      values: points.map((point) => seriesValue(point, sortKey)),
    };
  }, [points, sortKey]);
  return (
    <div style={{ width: SPARK_WIDTH, height: SPARK_HEIGHT }} data-project-spark={siteId}>
      {data === null ? null : (
        <ChartBoundary>
          <Suspense fallback={null}>
            <ChartSparkline
              t={data.t}
              values={data.values}
              width={SPARK_WIDTH}
              height={SPARK_HEIGHT}
            />
          </Suspense>
        </ChartBoundary>
      )}
    </div>
  );
}

interface RowProps {
  site: T.SiteStatsDto;
  sortKey: ProjectSortKey;
  /** prefers-reduced-motion: no shared-element transition (docs/07 section 2). */
  reduced: boolean;
}

const ProjectRow = memo(function ProjectRow({ site, sortKey, reduced }: RowProps) {
  if (import.meta.env.MODE === "qa") countRender("project", site.site_id);
  const { t } = useTranslation("metrics");
  const f = useMetricsFormatters();
  return (
    <Link
      to={DETAIL_ROUTE}
      params={{ id: site.site_id }}
      search={PERFORMANCE_SEARCH}
      // The host cell and the detail page's heading share this name: a shared-element transition.
      viewTransition={!reduced}
      data-shared-transition={String(!reduced)}
      data-project-row={site.site_id}
      aria-label={t("projects.open", { host: site.host })}
      className={`grid h-full items-center gap-2 border-b border-stroke px-3 text-body hover:bg-fill-hover ${COLUMNS} ${FOCUS_RING}`}
    >
      <span
        className="min-w-0 truncate text-fg-primary"
        style={reduced ? undefined : { viewTransitionName: `site-host-${site.site_id}` }}
      >
        {site.host}
      </span>
      <span className={`text-right ${f.tnum}`}>
        {f.approx(f.percent(site.cpu_pct), site.approx)}
      </span>
      <span className={`text-right ${f.tnum}`}>
        {f.approx(f.bytes(site.mem_bytes), site.approx)}
      </span>
      <span className={`text-right ${f.tnum}`}>{f.ms(site.p99_ms)}</span>
      <span className={`text-right ${f.tnum}`}>{f.percent(site.error_pct)}</span>
      <Sparkline siteId={site.site_id} sortKey={sortKey} />
    </Link>
  );
});

function IdleRow({ entry }: { entry: IdleSite }) {
  const { t } = useTranslation("metrics");
  return (
    <li
      data-idle-site={entry.site.site_id}
      className="flex flex-wrap items-center gap-2 border-b border-stroke px-3 py-2 last:border-b-0"
    >
      <span className="min-w-0 flex-1 truncate text-body text-fg-primary">{entry.site.host}</span>
      <Badge tone="neutral">
        {entry.idleDays === null
          ? t("projects.idle.never")
          : t("projects.idle.days", { count: entry.idleDays })}
      </Badge>
      <span className="text-callout text-fg-secondary">
        {t("projects.idle.processes", { count: entry.site.processes ?? 0 })}
      </span>
      <div className="min-w-0 basis-full sm:basis-auto">
        <CliCommand command={entry.stopCli} />
      </div>
    </li>
  );
}

function IdleSites({ sites }: { sites: readonly T.SiteStatsDto[] }) {
  const { t } = useTranslation("metrics");
  const now = useNow(CLOCK_MS);
  const idle = useMemo(() => idleSites(sites, now), [sites, now]);
  if (idle.length === 0) return null;
  return (
    <div data-testid="projects-idle" className="mt-3">
      <h3 className="pb-1 text-callout font-semibold text-fg-secondary">
        {t("projects.idle.title", { days: IDLE_DAYS })}
      </h3>
      <p className="pb-2 text-callout text-fg-secondary">{t("projects.idle.hint")}</p>
      <ul className="rounded-card border border-stroke bg-surface">
        {idle.slice(0, IDLE_SHOWN).map((entry) => (
          <IdleRow key={entry.site.site_id} entry={entry} />
        ))}
      </ul>
      {idle.length > IDLE_SHOWN ? (
        <p className="pt-1 text-callout text-fg-secondary">
          {t("projects.idle.more", { count: idle.length - IDLE_SHOWN })}
        </p>
      ) : null}
    </div>
  );
}

export function ProjectsSection() {
  const { t } = useTranslation("metrics");
  const runtime = useRuntime();
  const reduced = useReducedMotion();
  const status = useConnectionStatus();
  const capability = useCapability("stats.get");
  const stats = useAllSiteStats(WINDOW);
  const [sort, setSort] = useState<{ key: ProjectSortKey; direction: SortDirection }>({
    key: "cpu",
    direction: "desc",
  });
  const sites = stats.data?.sites;
  const ranked = useMemo(
    () => (sites === undefined ? [] : sortProjects(sites, sort.key, sort.direction)),
    [sites, sort],
  );
  const away = AWAY.has(status);
  const unavailable = !capability.available;
  const loading = !away && !unavailable && stats.isPending;
  const choose = (key: ProjectSortKey) => {
    setSort((previous) => ({
      key,
      direction: previous.key === key && previous.direction === "desc" ? "asc" : "desc",
    }));
  };
  return (
    <section aria-labelledby="dashboard-projects" data-testid="dashboard-projects">
      <div className="flex flex-wrap items-center gap-3 pb-2">
        <h2 id="dashboard-projects" className="text-headline">
          {t("projects.title")}
        </h2>
        <Badge tone="neutral">{t("windows.24h")}</Badge>
        <div className="ml-auto min-w-0">
          <CliCommand command={renderCli("stats.get", { window: WINDOW })} />
        </div>
      </div>
      <div className="rounded-card border border-stroke bg-surface">
        <div
          role="group"
          aria-label={t("projects.sortBy")}
          className={`grid items-center gap-2 border-b border-stroke px-3 py-1 text-callout text-fg-secondary ${COLUMNS}`}
        >
          <span>{t("projects.columns.site")}</span>
          {SORT_KEYS.map((key) => {
            const active = sort.key === key;
            return (
              <button
                key={key}
                type="button"
                data-sort={key}
                aria-pressed={active}
                aria-label={t("projects.sort", { column: t(`projects.columns.${key}`) })}
                onClick={() => {
                  choose(key);
                }}
                className={`rounded-control text-right ${active ? "font-semibold text-fg-primary" : ""} ${FOCUS_RING}`}
              >
                {t(`projects.columns.${key}`)}
                {active ? ARROWS[sort.direction] : ""}
              </button>
            );
          })}
          <span className="text-right">{t("projects.columns.trend")}</span>
        </div>
        <AsyncView
          height={LIST_HEIGHT}
          loading={loading}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(stats.error === null || unavailable ? {} : { error: stats.error })}
          errorContext={STATS_CONTEXT}
          onRetry={() => {
            void stats.refetch();
          }}
          empty={unavailable || ranked.length === 0}
          emptyTitle={unavailable ? t("projects.unavailable.title") : t("projects.empty.title")}
          emptyDescription={
            unavailable ? t("projects.unavailable.description") : t("projects.empty.description")
          }
        >
          <VirtualList
            items={ranked}
            rowHeight={ROW_HEIGHT}
            overscan={3}
            initialHeight={LIST_HEIGHT}
            label={t("projects.listLabel")}
            getKey={(site) => site.site_id}
            renderRow={(site) => <ProjectRow site={site} sortKey={sort.key} reduced={reduced} />}
          />
        </AsyncView>
      </div>
      <p className="pt-1 text-callout text-fg-secondary">{t("projects.approx")}</p>
      {sites === undefined ? null : <IdleSites sites={sites} />}
    </section>
  );
}
