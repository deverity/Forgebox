import { Outlet, useRouterState } from "@tanstack/react-router";
import { Suspense, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { useHello, useRuntime } from "../../state";
import { featureAt } from "../features";
import { PaletteHost } from "../palette/PaletteHost";
import { ConnectionBanner } from "./ConnectionBanner";
import { accentOf } from "./contexts";
import { LazyToasts } from "./LazyToasts";
import { Sidebar } from "./Sidebar";
import { StartAll } from "./StartAll";
import { Titlebar } from "./Titlebar";
import { useTrayBridge } from "./trayBridge";
import { useSidebarCollapsed } from "./useViewportCollapse";

/** The toolbar title: the name of the section that owns the current route. */
export function useSectionTitle(): string | undefined {
  const { t } = useTranslation("shell");
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const feature = featureAt(pathname);
  return feature === null ? undefined : t(`nav.${feature.id}`);
}

/**
 * Sidebar over the window material, full height from the top edge, next to the opaque content
 * pane with its own toolbar (docs/07 section 4, M2-E1-T23). There is no status bar: the daemon,
 * CPU/RAM, DNS and CA live in the sidebar footer. Renders before the daemon answers; the banner
 * says what the connection is doing.
 */
export function AppShell() {
  const context = useRuntime().context;
  // The native tray opens routes here and learns prefers-reduced-motion (docs/07 section 7).
  useTrayBridge();
  const title = useSectionTitle();
  const narrow = useSidebarCollapsed();
  // `hello.local === false` is the daemon telling it is on another machine (docs/13 section 5).
  const remote = useHello()?.local === false;
  const accent = accentOf(context);
  // A remote context re-tints the window accent (docs/13 section 5); `local` keeps the system's.
  const style = accent === null ? undefined : ({ "--accent": accent } as CSSProperties);
  return (
    <div
      data-testid="app-shell"
      data-context={context}
      data-remote={remote ? "true" : undefined}
      style={style}
      className="flex h-full bg-canvas text-fg-primary"
    >
      <Sidebar />
      <div
        data-testid="content-pane"
        className="flex min-w-0 flex-1 flex-col bg-(--bg-canvas-solid)"
      >
        <Titlebar title={title} primaryAction={<StartAll collapsed={narrow} />} />
        <main className="flex min-h-0 flex-1 flex-col">
          <ConnectionBanner />
          <Outlet />
        </main>
      </div>
      {/* The shortcuts' labels live in the `palette` namespace, which is not loaded with the
          shell: while it loads only this boundary waits. Without it the suspension reaches the
          shell route and the pending fallback replaces the whole window on first paint. */}
      <Suspense fallback={null}>
        <PaletteHost />
      </Suspense>
      <LazyToasts />
    </div>
  );
}
