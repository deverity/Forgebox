import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { shellRoute } from "./root";

export const phpRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/php",
  component: lazyRouteComponent(async () => {
    // The screen's texts load with its chunk, not with the shell. The update policy picker and
    // the quick-settings form come from the Services screen, so its texts are needed too.
    await i18n.loadNamespaces(["php", "services"]);
    return import("../../features/php");
  }, "PhpPage"),
});
