import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type uPlot from "uplot";
import { Badge } from "../../../design/primitives/Badge";
import { ChartShell, DEFAULT_CHART_HEIGHT } from "./ChartFrame";
import { baseOptions, memoFormat } from "./options";
import { APPROX } from "../format";
import { useMetricsFormatters } from "../state";
import { AXIS_FONT_PX, SWATCH } from "./palette";
import { aligned, summarize } from "./summary";
import type { BuildOptions } from "./UPlotCanvas";
import type { ChartMarker, ResourceSeries } from "./types";

/** Right-hand axis side (a const enum cannot be imported under isolatedModules). */
// eslint-disable-next-line @typescript-eslint/no-unsafe-enum-assignment -- uPlot's Side is a const enum
const RIGHT: uPlot.Axis.Side = 1;

export interface ResourceChartProps {
  series: ResourceSeries | null;
  markers?: readonly ChartMarker[];
  height?: number;
  loading?: boolean;
  className?: string;
}

/**
 * CPU (left axis, 0-100 %) and RAM (right axis, bytes). Per-site figures are attributed from the
 * pool's processes, so the title carries a "≈" badge (docs/14 section 3.3).
 */
export default function ResourceChart({
  series,
  markers,
  height = DEFAULT_CHART_HEIGHT,
  loading,
  className,
}: ResourceChartProps) {
  const { t, i18n } = useTranslation("metrics");
  const mf = useMetricsFormatters();
  const locale = i18n.language;
  const pct = useMemo(() => memoFormat((v) => mf.percent(v)), [mf]);
  const bytes = useMemo(() => memoFormat((v) => mf.bytes(v)), [mf]);

  const build = useCallback<BuildOptions>(
    (p) => {
      const base = baseOptions(p, locale, [], pct);
      const ramText = memoFormat(bytes);
      return {
        ...base,
        axes: [
          ...(base.axes ?? []),
          {
            scale: "ram",
            side: RIGHT,
            stroke: p.axis,
            font: `${String(AXIS_FONT_PX)}px ${p.fontFamily}`,
            size: 64,
            grid: { show: false },
            ticks: { stroke: p.grid, width: 1 },
            values: (_u, splits) => splits.map(ramText),
          },
        ],
        scales: {
          x: { time: true },
          y: { range: [0, 100] },
          ram: { range: (_u, _min, max) => [0, Math.max(max, 1) * 1.1] },
        },
        series: [
          {},
          { label: "CPU", scale: "y", stroke: p.accent, width: 1.5, points: { show: false } },
          { label: "RAM", scale: "ram", stroke: p.info, width: 1.5, points: { show: false } },
        ],
      };
    },
    [locale, pct, bytes],
  );

  const data = useMemo(
    () => (series ? aligned(series.t, series.cpu, series.ram) : aligned([])),
    [series],
  );
  const summary = useMemo(
    () => (series ? [summarize("CPU", series.cpu, pct, SWATCH.accent),
          summarize("RAM", series.ram, bytes, SWATCH.info),] : []),
    [series, pct, bytes],
  );

  return (
    <ChartShell
      title={t("charts.resource.title")}
      height={height}
      loading={loading ?? false}
      empty={!series || series.t.length === 0}
      build={build}
      data={data}
      summary={summary}
      markers={markers ?? []}
      className={className}
      badge={
        <span title={t("approx")} data-chart-approx="">
          <Badge>
            <span aria-hidden="true">{APPROX}</span>
            <span className="sr-only">{t("approx")}</span>
          </Badge>
        </span>
      }
    />
  );
}
