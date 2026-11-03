import { useTranslation } from "react-i18next";
import { FOCUS_RING } from "../design/primitives/shared";
import { SUPPORTED_LOCALES, setLanguage, type Locale } from "./index";

/** Runtime language picker: re-renders without reload and syncs Rust through `set_language`. */
export function LanguageSwitcher() {
  const { t, i18n } = useTranslation("common");
  return (
    <label className="flex items-center gap-2 text-body">
      <span>{t("language.label")}</span>
      <select
        className={FOCUS_RING}
        value={i18n.language}
        onChange={(event) => {
          void setLanguage(event.target.value as Locale);
        }}
      >
        {SUPPORTED_LOCALES.map((locale) => (
          <option key={locale} value={locale}>
            {t(`language.${locale}`)}
          </option>
        ))}
      </select>
    </label>
  );
}
