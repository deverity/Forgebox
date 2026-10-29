import { Outlet } from "@tanstack/react-router";
import type { CSSProperties } from "react";
import { useRuntime } from "../../state";
import { ConnectionBanner } from "./ConnectionBanner";
import { accentOf } from "./contexts";
import { LazyToasts } from "./LazyToasts";
import { Sidebar } from "./Sidebar";
import { StatusBar } from "./StatusBar";
import { Titlebar } from "./Titlebar";

/**
 * Titlebar, sidebar (window material), opaque content and status bar (docs/07 section 4).
 * Renders before the daemon answers; the banner says what the connection is doing.
 */
export function AppShell() {
  const context = useRuntime().context;
  const accent = accentOf(context);
  // A remote context re-tints the window accent (docs/13 section 5); `local` keeps the system's.
  const style = accent === null ? undefined : ({ "--accent": accent } as CSSProperties);
  return (
    <div
      data-testid="app-shell"
      data-context={context}
      style={style}
      className="flex h-full flex-col bg-canvas text-fg-primary"
    >
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <main className="flex min-w-0 flex-1 flex-col bg-(--bg-canvas-solid)">
          <ConnectionBanner />
          <Outlet />
        </main>
      </div>
      <StatusBar />
      <LazyToasts />
    </div>
  );
}
