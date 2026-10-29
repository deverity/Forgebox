import { Suspense, lazy } from "react";
import { useTranslation } from "react-i18next";
import i18n from "i18next";
import { Titlebar } from "./app/shell/Titlebar";
import { useFirstFrameReady } from "./app/shell/windowApi";
import { LanguageSwitcher } from "./i18n/LanguageSwitcher";
import type { Bridge } from "./ipc/bridge";

export interface AppProps {
  /**
   * Bridge chosen by the entry point: the QA fake bridge in `qa` mode (ADR-0036), otherwise
   * undefined and the state layer (M2-E1-T10) uses createTauriBridge(). Unused by the placeholder.
   */
  bridge?: Bridge | undefined;
}

// Dev/QA route, code-split so it never weighs on the initial bundle. The MODE check is replaced
// statically by Vite, so the production build drops the chunk entirely (dev, test and qa keep it).
// M2-E1-T10 moves it into the real router; until then the pathname is checked directly.
const Gallery =
  import.meta.env.MODE === "production"
    ? null
    : lazy(async () => {
        await i18n.loadNamespaces("gallery");
        return import("./app/gallery/Gallery");
      });
export const GALLERY_PATH = "/__gallery";

/**
 * Placeholder shell: the window chrome (M2-E1-T7) around placeholder content. The real layout
 * and router arrive in M2-E1-T10; whatever replaces this keeps the Titlebar and calls
 * useFirstFrameReady() in the first committed component (the window is shown on `ready`).
 */
export function App({ bridge }: AppProps = {}) {
  const { t } = useTranslation("shell");
  // Every hook runs before the gallery early return (rules of hooks).
  useFirstFrameReady();
  if (Gallery !== null && window.location.pathname === GALLERY_PATH) {
    return (
      <Suspense fallback={null}>
        <Gallery />
      </Suspense>
    );
  }
  return (
    <div
      className="flex h-full flex-col bg-canvas text-fg-primary"
      data-bridge={bridge ? "injected" : "default"}
    >
      <Titlebar />
      <main className="flex flex-1 flex-col items-center justify-center gap-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <p className="text-body">{t("tagline")}</p>
        <LanguageSwitcher />
      </main>
    </div>
  );
}
