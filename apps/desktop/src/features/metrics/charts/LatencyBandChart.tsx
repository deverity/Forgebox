import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type uPlot from "uplot";
import { ChartShell, DEFAULT_CHART_HEIGHT } from "./ChartFrame";
import { baseOptions, memoFormat } from "./options";
import { useMetricsFormatters } from "../state";
import { SWATCH, withAlpha } from "./palette";
import { aligned, summarize } from "./summary";
import type { BuildOptions } from "./UPlotCanvas";
import type { ChartMarker, LatencySeries } from "./types";

export interface LatencyBandChartProps {
  series: LatencySeries | null;
  markers?: readonly ChartMarker[];
  height?: number;
  loading?: boolean;
  className?: string;
}

/** p50-p90-p99 latency as nested bands, plus an optional PHP-time line (docs/14 section 4.1). */
export default function LatencyBandChart({
  series,
  markers,
  height = DEFAULT_CHART_HEIGHT,
  loading,
  className,
}: LatencyBandChartProps) {
  const { t, i18n } = useTranslation("metrics");
  const mf = useMetricsFormatters();
  const locale = i18n.language;
  const hasPhp = series?.php !== undefined;
  const ms = useMemo(() => memoFormat((v) => mf.ms(v)), [mf]);
  const phpLabel = t("stat.php");

  const build = useCallback<BuildOptions>(
    (p) => {
      const line = (
        label: string,
        stroke: string,
        extra: Partial<uPlot.Series> = {},
      ): uPlot.Series => ({
        label,
        stroke,
        width: 1.5,
        points: { show: false },
        ...extra,
      });
      return {
        ...baseOptions(p, locale, [], ms),
        scales: {
          x: { time: true },
          y: { range: (_u, _min, max) => [0, Math.max(max, 1) * 1.1] },
        },
        series: [
          {},
          line("p50", p.accent),
          line("p90", p.warn),
          line("p99", p.danger),
          ...(hasPhp ? [line(phpLabel, p.info, { dash: [4, 4], width: 1 })] : []),
        ],
        bands: [
          { series: [3, 2], fill: withAlpha(p.danger, 16) },
          { series: [2, 1], fill: withAlpha(p.warn, 16) },
        ],
      };
    },
    [locale, ms, hasPhp, phpLabel],
  );

  const data = useMemo(
    () =>
      series
        ? aligned(series.t, series.p50, series.p90, series.p99, ...(series.php ? [series.php] : []))
        : aligned([]),
    [series],
  );
  const summary = useMemo(
    () =>
      series
        ? [
            summarize("p50", series.p50, ms, SWATCH.accent),
            summarize("p90", series.p90, ms, SWATCH.warn),
            summarize("p99", series.p99, ms, SWATCH.danger),
            ...(series.php ? [summarize(phpLabel, series.php, ms, SWATCH.info)] : []),
          ]
        : [],
    [series, ms, phpLabel],
  );

  return (
    <ChartShell
      title={t("charts.latency.title")}
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
