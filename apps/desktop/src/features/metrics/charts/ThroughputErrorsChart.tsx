import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type uPlot from "uplot";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { ChartShell, DEFAULT_CHART_HEIGHT } from "./ChartFrame";
import { baseOptions } from "./options";
import { withAlpha } from "./palette";
import { aligned, summarize, sumColumns } from "./summary";
import type { BuildOptions } from "./UPlotCanvas";
import type { ChartMarker, ThroughputSeries } from "./types";

export interface ThroughputErrorsChartProps {
  series: ThroughputSeries | null;
  markers?: readonly ChartMarker[];
  height?: number;
  loading?: boolean;
  className?: string;
}

/** Requests per second as a line, with 4xx and 5xx stacked as areas under it. */
export default function ThroughputErrorsChart({
  series,
  markers,
  height = DEFAULT_CHART_HEIGHT,
  loading,
  className,
}: ThroughputErrorsChartProps) {
  const { t, i18n } = useTranslation("metrics");
  const fmt = useFormatters();
  const locale = i18n.language;
  const rps = useCallback((v: number) => fmt.number(v, { maximumFractionDigits: 1 }), [fmt]);
  const rpsLabel = t("series.rps");

  const build = useCallback<BuildOptions>(
    (p) => {
      const area = (label: string, color: string): uPlot.Series => ({
        label,
        stroke: color,
        width: 1,
        fill: withAlpha(color, 35),
        points: { show: false },
      });
      return {
        ...baseOptions(p, locale, [], rps),
        scales: {
          x: { time: true },
          y: { range: (_u, _min, max) => [0, Math.max(max, 1) * 1.1] },
        },
        series: [
          {},
          // The cumulative (4xx + 5xx) area is drawn first and the 4xx area paints over its
          // lower part: uPlot has no stacking, this gives the stacked look.
          area("5xx", p.danger),
          area("4xx", p.warn),
          { label: rpsLabel, stroke: p.accent, width: 1.5, points: { show: false } },
        ],
      };
    },
    [locale, rps, rpsLabel],
  );

  const data = useMemo(
    () =>
      series
        ? aligned(series.t, sumColumns(series.e4xx, series.e5xx), series.e4xx, series.rps)
        : aligned([]),
    [series],
  );
  const summary = useMemo(
    () =>
      series
        ? [
            summarize(rpsLabel, series.rps, rps),
            summarize("4xx", series.e4xx, rps),
            summarize("5xx", series.e5xx, rps),
          ]
        : [],
    [series, rps, rpsLabel],
  );

  return (
    <ChartShell
      title={t("throughput.title")}
      height={height}
      loading={loading ?? false}
      empty={!series || series.t.length === 0}
      build={build}
      data={data}
      summary={summary}
      markers={markers ?? []}
      className={className}
    />
  );
}
