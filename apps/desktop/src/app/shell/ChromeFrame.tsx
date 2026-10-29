import { Outlet } from "@tanstack/react-router";
import { ConnectionBanner } from "./ConnectionBanner";
import { LazyToasts } from "./LazyToasts";
import { Titlebar } from "./Titlebar";

/** The window chrome alone (titlebar over an opaque canvas), for screens without the sidebar. */
export function ChromeFrame() {
  return (
    <div className="flex h-full flex-col bg-canvas text-fg-primary">
      <Titlebar />
      <main className="flex min-h-0 flex-1 flex-col bg-(--bg-canvas-solid)">
        <ConnectionBanner />
        <Outlet />
      </main>
      <LazyToasts />
    </div>
  );
}
