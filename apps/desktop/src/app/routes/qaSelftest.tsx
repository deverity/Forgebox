import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { SELFTEST_PATH } from "../../qa/selftest/cases";
import { rootRoute } from "./root";

/**
 * The qa:check negative self-tests (M2-E4-T3): qa builds only. Vite replaces the MODE check
 * statically, so dev, test and production builds drop the route and its chunk.
 */
export const qaSelftestRoute =
  import.meta.env.MODE === "qa"
    ? createRoute({
        getParentRoute: () => rootRoute,
        path: SELFTEST_PATH,
        component: lazyRouteComponent(() => import("../../qa/selftest/SelfTest"), "default"),
      })
    : null;
