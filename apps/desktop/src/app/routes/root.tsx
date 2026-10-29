import {
  Outlet,
  createRoute,
  createRootRoute,
  redirect,
  useRouter,
  useRouterState,
} from "@tanstack/react-router";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { useConnectionStatus } from "../../state";
import { AppShell } from "../shell/AppShell";
import { useFirstFrameReady } from "../shell/windowApi";

export const ONBOARDING_PATH = "/onboarding";
export const GALLERY_PATH = "/__gallery";
export const HOME_PATH = "/dashboard";

/** Paths the onboarding gate leaves alone. */
const UNGATED = new Set([ONBOARDING_PATH, GALLERY_PATH]);

/**
 * The first-run gate (docs/07 section 5.1): a root that was never initialized has nothing to
 * show but onboarding. Everything else (including a stopped daemon) stays navigable.
 */
function OnboardingGate() {
  const status = useConnectionStatus();
  const router = useRouter();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  useEffect(() => {
    if (status === "uninitialized" && !UNGATED.has(pathname)) {
      void router.navigate({ to: ONBOARDING_PATH, replace: true });
    }
  }, [status, pathname, router]);
  return null;
}

function RootLayout() {
  // The window is shown once the first component has committed (ADR-0035).
  useFirstFrameReady();
  return (
    <>
      <OnboardingGate />
      <Outlet />
    </>
  );
}

function NotFound() {
  const { t } = useTranslation("shell");
  const router = useRouter();
  return (
    <EmptyState
      title={t("routes.notFound")}
      action={
        <Button
          onClick={() => {
            void router.navigate({ to: HOME_PATH });
          }}
        >
          {t("routes.goHome")}
        </Button>
      }
    />
  );
}

export const rootRoute = createRootRoute({ component: RootLayout, notFoundComponent: NotFound });

/** Pathless layout: titlebar, sidebar, content and status bar around the feature routes. */
export const shellRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "shell",
  component: AppShell,
});

export const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  beforeLoad: () => {
    // eslint-disable-next-line @typescript-eslint/only-throw-error -- TanStack Router's redirect
    throw redirect({ to: HOME_PATH, replace: true });
  },
});
