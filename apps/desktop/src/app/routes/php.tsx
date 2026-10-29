import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const phpRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/php",
  component: lazyRouteComponent(() => import("../../features/php"), "PhpPage"),
});
