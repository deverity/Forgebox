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
  const { layout, onDraw } = useMarkerLayout(markers);
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
      </figcaption>
      <SkeletonSlot loading={loading} height={height} label={t("loading")} className="relative">
        {empty ? (
          <EmptyState
            title={t("empty.title")}
            description={t("empty.description")}
            className="h-full p-4"
          />
        ) : (
          <>
            <UPlotCanvas build={build} data={data} height={height} onDraw={onDraw} />
            {markers.length > 0 ? <MarkersOverlay layout={layout} /> : null}
          </>
        )}
      </SkeletonSlot>
      {state === "ready" ? (
        <table className="sr-only" data-chart-summary="">
          <caption>{t("summary.caption", { title })}</caption>
          <thead>
            <tr>
              <th scope="col">{t("summary.series")}</th>
              <th scope="col">{t("summary.latest")}</th>
              <th scope="col">{t("summary.peak")}</th>
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
