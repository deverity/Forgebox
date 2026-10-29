import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const databasesRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/databases",
  component: lazyRouteComponent(() => import("../../features/databases"), "DatabasesPage"),
});
