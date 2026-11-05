import { Outlet } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { ConnectionBanner } from "./ConnectionBanner";
import { LazyToasts } from "./LazyToasts";
import { Titlebar } from "./Titlebar";

/**
 * The window chrome alone, for screens without the sidebar (onboarding): the same toolbar as
 * the content pane (product name on the left, search, window controls) over an opaque canvas.
 */
export function ChromeFrame() {
  const { t } = useTranslation("shell");
  return (
    <div className="flex h-full flex-col bg-(--bg-canvas-solid) text-fg-primary">
      <Titlebar title={t("title")} />
      <main className="flex min-h-0 flex-1 flex-col">
        <ConnectionBanner />
        <Outlet />
      </main>
      <LazyToasts />
    </div>
  );
}
