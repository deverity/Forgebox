import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { shellRoute } from "./root";

/** `/logs?target=nginx.error` opens the screen with that source selected. */
export interface LogsSearch {
  target?: string;
}

export const logsRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/logs",
  validateSearch: (search: Record<string, unknown>): LogsSearch =>
    typeof search.target === "string" && search.target !== "" ? { target: search.target } : {},
  component: lazyRouteComponent(async () => {
    await i18n.loadNamespaces("logs");
    return import("../../features/logs");
  }, "LogsPage"),
});
