import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { NO_VALUE } from "../format";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { withAlpha } from "./palette";
import { aligned, lastValue, peakValue } from "./summary";
import { UPlotCanvas, type BuildOptions } from "./UPlotCanvas";

export interface ChartSparklineProps {
  t: readonly number[];
  values: readonly (number | null)[];
  width?: number;
  height?: number;
  /** Formats the value in the accessible label. */
  format?: (value: number) => string;
}

/** Compact uPlot variant: no axes, grid or markers, fixed size, one token-coloured line. */
export default function ChartSparkline({
  t,
  values,
  width = 120,
  height = 32,
  format,
}: ChartSparklineProps) {
  const { t: tr } = useTranslation("metrics");
  const fmt = useFormatters();
  const f = format ?? ((v: number) => fmt.number(v, { maximumFractionDigits: 1 }));
  const build = useCallback<BuildOptions>(
    (p) => ({
      legend: { show: false },
      cursor: { show: false },
      select: { show: false, left: 0, top: 0, width: 0, height: 0 },
      padding: [2, 2, 2, 2],
      axes: [{ show: false }, { show: false }],
      scales: { x: { time: false }, y: { range: (_u, _min, max) => [0, Math.max(max, 1)] } },
      series: [
        {},
        { stroke: p.accent, width: 1.5, fill: withAlpha(p.accent, 14), points: { show: false } },
      ],
    }),
    [],
  );
  const data = useMemo(() => aligned(t, values), [t, values]);
  const latest = lastValue(values);
  const peak = peakValue(values);
  const label = tr("charts.sparkline.label", {
    count: values.length,
    latest: latest === null ? NO_VALUE : f(latest),
    peak: peak === null ? NO_VALUE : f(peak),
  });
  return (
    <div role="img" aria-label={label} data-chart-sparkline="" style={{ width, height }}>
      {values.length > 0 ? (
        <UPlotCanvas build={build} data={data} height={height} width={width} />
      ) : null}
    </div>
  );
}
