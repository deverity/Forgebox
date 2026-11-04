import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { CHARTS_PATH, rootRoute } from "./root";

/**
 * Harness of the metrics charts (M2-E5-T15): dev, test and qa builds only, like the gallery.
 * The production build drops the route and its chunk (MODE is replaced statically).
 */
export const chartsRoute =
  import.meta.env.MODE === "production"
    ? null
    : createRoute({
        getParentRoute: () => rootRoute,
        path: CHARTS_PATH,
        component: lazyRouteComponent(async () => {
          await i18n.loadNamespaces("metrics");
          return import("../../features/metrics/charts/ChartsHarness");
        }, "default"),
      });
