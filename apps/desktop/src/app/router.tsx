// The route tree (ADR-0034 section 6): TanStack Router, code-based, one route file per feature
// in `routes/`, every feature screen a lazy chunk. `/diagnostics` is routed but not in the
// sidebar; `/onboarding` is the first-run gate; `/__gallery` exists outside production builds.
import { createMemoryHistory, createRouter, type RouterHistory } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { SPINNER_INLINE } from "../design/primitives/shared";
import { catalogRoute, catalogToolRoute } from "./routes/catalog";
import { dashboardRoute } from "./routes/dashboard";
import { databasesRoute } from "./routes/databases";
import { diagnosticsRoute } from "./routes/diagnostics";
import { galleryRoute } from "./routes/gallery";
import { logsRoute } from "./routes/logs";
import { mailRoute } from "./routes/mail";
import { chromeRoute, onboardingRoute } from "./routes/onboarding";
import { phpRoute } from "./routes/php";
import { indexRoute, rootRoute, shellRoute } from "./routes/root";
import { servicesRoute } from "./routes/services";
import { settingsRoute } from "./routes/settings";
import { siteDetailRoute, sitesRoute } from "./routes/sites";
import { terminalRoute } from "./routes/terminal";
import "./slotModules";

/** Shown while a lazy screen loads: the spinner always comes with its text. */
function RoutePending() {
  const { t } = useTranslation("shell");
  return (
    <p role="status" className="flex items-center gap-2 p-4 text-callout text-fg-secondary">
      <span aria-hidden="true" className={SPINNER_INLINE} />
      {t("routes.loading")}
    </p>
  );
}

const routeTree = rootRoute.addChildren([
  indexRoute,
  chromeRoute.addChildren([onboardingRoute]),
  ...(galleryRoute === null ? [] : [galleryRoute]),
  shellRoute.addChildren([
    dashboardRoute,
    servicesRoute,
    sitesRoute,
    siteDetailRoute,
    phpRoute,
    databasesRoute,
    mailRoute,
    logsRoute,
    catalogRoute,
    catalogToolRoute,
    terminalRoute,
    settingsRoute,
    diagnosticsRoute,
  ]),
]);

export function createAppRouter(history?: RouterHistory) {
  return createRouter({
    routeTree,
    ...(history === undefined ? {} : { history }),
    defaultPendingComponent: RoutePending,
    defaultPendingMs: 150,
    defaultPreload: "intent",
  });
}

export type AppRouter = ReturnType<typeof createAppRouter>;

/** A router over an in-memory history (tests, and Storybook-style embedding). */
export function createTestRouter(initialPath = "/dashboard"): AppRouter {
  return createAppRouter(createMemoryHistory({ initialEntries: [initialPath] }));
}
