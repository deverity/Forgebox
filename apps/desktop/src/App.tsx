import { useTranslation } from "react-i18next";
import { LanguageSwitcher } from "./i18n/LanguageSwitcher";

/** Placeholder shell (M2-E1-T1). The real layout, router arrive in M2-E1-T7..T10. */
export function App() {
  const { t } = useTranslation("shell");
  return (
    <main className="flex h-full flex-col items-center justify-center gap-3 bg-canvas text-fg-primary">
      <h1 className="text-title-1">{t("title")}</h1>
      <p className="text-body">{t("tagline")}</p>
      <LanguageSwitcher />
    </main>
  );
}
