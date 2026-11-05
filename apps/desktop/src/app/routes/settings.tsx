import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { shellRoute } from "./root";

export const settingsRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/settings",
  component: lazyRouteComponent(async () => {
    // Texts load with the chunk (a suspending useTranslation would replace the shell).
    // `palette` holds the shortcut names, `updater` the autostart toggle, `errors` the causes.
    await i18n.loadNamespaces(["settings", "palette", "updater", "errors"]);
    return import("../../features/settings");
  }, "SettingsPage"),
});
