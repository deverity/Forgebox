import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const sitesRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/sites",
  component: lazyRouteComponent(() => import("../../features/sites"), "SitesPage"),
});

export const siteDetailRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/sites/$id",
  component: lazyRouteComponent(() => import("../../features/sites"), "SiteDetailPage"),
});
