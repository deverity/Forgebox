import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const servicesRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/services",
  component: lazyRouteComponent(() => import("../../features/services"), "ServicesPage"),
});
