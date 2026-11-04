import { Outlet } from "@tanstack/react-router";
import { Suspense, type CSSProperties } from "react";
import { useRuntime } from "../../state";
import { PaletteHost } from "../palette/PaletteHost";
import { ConnectionBanner } from "./ConnectionBanner";
import { accentOf } from "./contexts";
import { LazyToasts } from "./LazyToasts";
import { Sidebar } from "./Sidebar";
import { StatusBar } from "./StatusBar";
import { Titlebar } from "./Titlebar";
import { useTrayBridge } from "./trayBridge";

/**
 * Titlebar, sidebar (window material), opaque content and status bar (docs/07 section 4).
 * Renders before the daemon answers; the banner says what the connection is doing.
 */
export function AppShell() {
  const context = useRuntime().context;
  // The native tray opens routes here and learns prefers-reduced-motion (docs/07 section 7).
  useTrayBridge();
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
