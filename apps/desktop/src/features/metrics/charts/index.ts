// Public surface of the metrics charts (M2-E5-T15). uPlot is only reachable through these lazy
// wrappers, so it never lands in the initial bundle or in a route chunk that draws no chart.
// Types are safe to import eagerly.
import i18n from "i18next";
import { lazy } from "react";

async function withNamespace<T>(load: () => Promise<T>): Promise<T> {
  await i18n.loadNamespaces("metrics");
  return load();
}

export const LatencyBandChart = lazy(() => withNamespace(() => import("./LatencyBandChart")));
export const ThroughputErrorsChart = lazy(() =>
  withNamespace(() => import("./ThroughputErrorsChart")),
);
export const ResourceChart = lazy(() => withNamespace(() => import("./ResourceChart")));
export const ChartSparkline = lazy(() => withNamespace(() => import("./ChartSparkline")));

export type * from "./types";
