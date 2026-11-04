import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { ChromeFrame } from "../shell/ChromeFrame";
import { ONBOARDING_PATH, rootRoute } from "./root";

// The first-run gate: no sidebar (there is nothing to navigate to yet), only the window chrome.
export const chromeRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "chrome",
  component: ChromeFrame,
});

export const onboardingRoute = createRoute({
  getParentRoute: () => chromeRoute,
  path: ONBOARDING_PATH,
  component: lazyRouteComponent(async () => {
    // The plan summary and the install rows come from the Catalog screen, so its texts load too.
    await i18n.loadNamespaces(["onboarding", "catalog"]);
    return import("../../features/onboarding");
  }, "OnboardingPage"),
});
