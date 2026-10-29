import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const catalogRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/catalog",
  component: lazyRouteComponent(() => import("../../features/catalog"), "CatalogPage"),
});

export const catalogToolRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/catalog/$id",
  component: lazyRouteComponent(() => import("../../features/catalog"), "CatalogToolPage"),
});
