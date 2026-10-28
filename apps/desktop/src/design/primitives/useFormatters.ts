import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { createFormatters, type Formatters } from "../../i18n/format";

/** Formatters bound to the active UI language. */
export function useFormatters(): Formatters {
  const { i18n } = useTranslation();
  const language = i18n.language;
  return useMemo(() => createFormatters(language), [language]);
}
