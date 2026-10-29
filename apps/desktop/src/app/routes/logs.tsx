import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const logsRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/logs",
  component: lazyRouteComponent(() => import("../../features/logs"), "LogsPage"),
});
