import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const diagnosticsRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/diagnostics",
  component: lazyRouteComponent(() => import("../../features/diagnostics"), "DiagnosticsPage"),
});
