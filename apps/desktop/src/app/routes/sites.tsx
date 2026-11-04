import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

/** `/sites?php=8.4` lists only the sites served by that PHP series (PHP screen link). */
export interface SitesSearch {
  php?: string;
}

export const sitesRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/sites",
  // The router JSON-parses the query, so a hand-typed `?php=8.4` arrives as the number 8.4.
  validateSearch: (search: Record<string, unknown>): SitesSearch => {
    const php = typeof search.php === "number" ? String(search.php) : search.php;
    return typeof php === "string" && php !== "" ? { php } : {};
  },
  component: lazyRouteComponent(() => import("../../features/sites"), "SitesPage"),
});

export const siteDetailRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/sites/$id",
  component: lazyRouteComponent(() => import("../../features/sites"), "SiteDetailPage"),
});
