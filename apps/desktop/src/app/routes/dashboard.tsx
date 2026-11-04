import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { shellRoute } from "./root";

export const dashboardRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/dashboard",
  component: lazyRouteComponent(async () => {
    // The screen's texts load with its chunk, not with the shell. The service tiles reuse the
    // Services screen's state labels, so its texts are needed too. Loading them here (and not
    // through a suspending `useTranslation`) keeps the shell on screen while the default route
    // loads: a suspension in the page would replace the whole shell with the pending fallback.
    await i18n.loadNamespaces(["dashboard", "services"]);
    return import("../../features/dashboard");
  }, "DashboardPage"),
});
