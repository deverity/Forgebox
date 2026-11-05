import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { EmptyState } from "../../../design/primitives/EmptyState";
import { SkeletonSlot } from "../../../design/primitives/Skeleton";
import { MarkersOverlay, useMarkerLayout } from "./MarkersOverlay";
import { UPlotCanvas, type BuildOptions } from "./UPlotCanvas";
import type uPlot from "uplot";
import type { ChartMarker, SummaryRow } from "./types";

export const DEFAULT_CHART_HEIGHT = 200;

export interface ChartShellProps {
  title: string;
  height?: number;
  loading?: boolean;
  /** No points to draw. */
  empty: boolean;
  build: BuildOptions;
  data: uPlot.AlignedData;
  summary: readonly SummaryRow[];
  markers?: readonly ChartMarker[];
  /** Extra badge next to the title (the resource chart's approximation mark). */
  badge?: ReactNode;
  className?: string | undefined;
}

/**
 * Compact key for sighted users: a token-coloured swatch, the series name and its latest value.
 * Decorative (`aria-hidden`): the summary table below carries the same information for
 * assistive technology.
 */
export function ChartLegend({ rows }: { rows: readonly SummaryRow[] }) {
  const items = rows.filter((row) => row.swatch !== undefined);
  if (items.length === 0) return null;
  return (
    <ul
      aria-hidden="true"
      data-chart-legend=""
      className="m-0 ml-auto flex list-none flex-wrap justify-end gap-x-3 gap-y-1 p-0 text-caption"
    >
      {items.map((row) => (
        <li key={row.label} className="flex items-center gap-1">
          <span data-legend-swatch="" className={`size-2 rounded-full ${row.swatch ?? ""}`} />
          <span>{row.label}</span>
          <span className="text-fg-primary tabular-nums">{row.latest}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Shared frame of the three charts: a <figure> with a fixed-height slot (skeleton while loading,
 * empty state without points, canvas otherwise), the markers overlay and an accessible summary
 * table (the canvas itself is aria-hidden).
 */
export function ChartShell({
  title,
  height = DEFAULT_CHART_HEIGHT,
  loading = false,
  empty,
  build,
  data,
  summary,
  markers = [],
  badge,
  className,
}: ChartShellProps) {
  const { t } = useTranslation("metrics");
  const { layout, onDraw, overlayRef } = useMarkerLayout(markers);
  const state = loading ? "loading" : empty ? "empty" : "ready";

  return (
    <figure
      data-chart=""
      data-chart-state={state}
      className={["m-0 flex flex-col gap-2", className].filter(Boolean).join(" ")}
    >
      <figcaption className="flex items-center gap-2 text-callout text-fg-secondary">
        <span>{title}</span>
        {badge}
        {state === "ready" ? <ChartLegend rows={summary} /> : null}
      </figcaption>
      <SkeletonSlot
        loading={loading}
        height={height}
        label={t("charts.loading")}
        className="relative"
      >
        {empty ? (
          <EmptyState
            title={t("charts.empty.title")}
            description={t("charts.empty.description")}
            className="h-full p-4"
          />
        ) : (
          <>
            <UPlotCanvas build={build} data={data} height={height} onDraw={onDraw} />
            {markers.length > 0 ? <MarkersOverlay layout={layout} overlayRef={overlayRef} /> : null}
          </>
        )}
      </SkeletonSlot>
      {state === "ready" ? (
        <table className="sr-only" data-chart-summary="">
          <caption>{t("charts.summary.caption", { title })}</caption>
          <thead>
            <tr>
              <th scope="col">{t("charts.summary.series")}</th>
              <th scope="col">{t("charts.summary.latest")}</th>
              <th scope="col">{t("charts.summary.peak")}</th>
            </tr>
          </thead>
          <tbody>
            {summary.map((row) => (
              <tr key={row.label}>
                <th scope="row">{row.label}</th>
                <td>{row.latest}</td>
                <td>{row.peak}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </figure>
  );
}
