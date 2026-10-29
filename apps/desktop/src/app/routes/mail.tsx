import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import { shellRoute } from "./root";

export const mailRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/mail",
  component: lazyRouteComponent(() => import("../../features/mail"), "MailPage"),
});
