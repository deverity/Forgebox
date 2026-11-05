import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { shellRoute } from "./root";

/** `/services?instance=<id>` opens the detail sheet of that instance (palette, deep links). */
export interface ServicesSearch {
  instance?: string;
}

export const servicesRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/services",
  validateSearch: (search: Record<string, unknown>): ServicesSearch =>
    typeof search.instance === "string" && search.instance !== ""
      ? { instance: search.instance }
      : {},
  component: lazyRouteComponent(async () => {
    // The screen's texts load with its chunk, not with the shell.
    await i18n.loadNamespaces(["services", "logs"]);
    return import("../../features/services");
  }, "ServicesPage"),
});
