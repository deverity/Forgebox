import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const terminalRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/terminal",
  component: lazyRouteComponent(() => import("../../features/terminal"), "TerminalPage"),
});
