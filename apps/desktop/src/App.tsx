import { Suspense, lazy } from "react";
import { useTranslation } from "react-i18next";
import i18n from "i18next";
import { LanguageSwitcher } from "./i18n/LanguageSwitcher";

// Dev/QA route, code-split so it never weighs on the initial bundle. M2-E1-T10 moves it into the
// real router; until then the pathname is checked directly.
const Gallery = lazy(async () => {
  await i18n.loadNamespaces("gallery");
  return import("./app/gallery/Gallery");
});
export const GALLERY_PATH = "/__gallery";

/** Placeholder shell (M2-E1-T1). The real layout, router arrive in M2-E1-T7..T10. */
export function App() {
  const { t } = useTranslation("shell");
  if (window.location.pathname === GALLERY_PATH) {
    return (
      <Suspense fallback={null}>
        <Gallery />
      </Suspense>
    );
  }
  return (
    <main className="flex h-full flex-col items-center justify-center gap-3 bg-canvas text-fg-primary">
      <h1 className="text-title-1">{t("title")}</h1>
      <p className="text-body">{t("tagline")}</p>
      <LanguageSwitcher />
    </main>
  );
}
