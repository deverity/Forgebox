import { useTranslation } from "react-i18next";
import { Select } from "../design/primitives/Select";
import { saveLanguage } from "../features/settings/guiPrefs";
import { SUPPORTED_LOCALES, setLanguage, type Locale } from "./index";

/**
 * Runtime language picker: re-renders without reload and syncs Rust through `set_language`.
 * The design-system Select (R-30), named for assistive technology only: the Settings row
 * already shows the visible "Idioma", so no second label sits beside it.
 */
export function LanguageSwitcher() {
  const { t, i18n } = useTranslation("common");
  return (
    <Select
      label={t("language.label")}
      width="sm"
      value={i18n.language}
      options={SUPPORTED_LOCALES.map((locale) => ({
        value: locale,
        label: t(`language.${locale}`),
      }))}
      onValueChange={(value) => {
        const locale = value as Locale;
        void setLanguage(locale);
        void saveLanguage(locale);
      }}
    />
  );
}
