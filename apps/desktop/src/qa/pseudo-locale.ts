// `?locale=` of qa-mode builds. The pseudo-locale itself (accents, +40 % length, brackets) is
// src/i18n/pseudo.ts; this only maps the URL value to an i18n override.
import { PSEUDO_LOCALE, SUPPORTED_LOCALES, type AnyLocale } from "../i18n";

/** `pseudo`, a shipped locale (case-insensitive) or null (follow the OS). */
export function qaLocaleOverride(value: string | null): AnyLocale | null {
  if (value === null) return null;
  if (value.toLowerCase() === PSEUDO_LOCALE) return PSEUDO_LOCALE;
  return SUPPORTED_LOCALES.find((l) => l.toLowerCase() === value.toLowerCase()) ?? null;
}
