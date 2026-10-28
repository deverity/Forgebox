import { useTranslation } from "react-i18next";
import { LanguageSwitcher } from "./i18n/LanguageSwitcher";

/** Placeholder shell (M2-E1-T1). The real layout, router and tokens arrive in M2-E1-T6..T10. */
export function App() {
  const { t } = useTranslation("shell");
  return (
    <main className="flex h-full flex-col items-center justify-center gap-3 bg-neutral-50 text-neutral-900 dark:bg-neutral-900 dark:text-neutral-50">
      <h1 className="text-2xl font-semibold">{t("title")}</h1>
      <p className="text-sm">{t("tagline")}</p>
      <LanguageSwitcher />
    </main>
  );
}
