import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
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
  component: lazyRouteComponent(() => import("../../features/onboarding"), "OnboardingPage"),
});
