import i18next from "eslint-plugin-i18next";

/**
 * Words that may appear as literal JSX text: brand and product names, binaries and forge
 * commands (CLAUDE.md "Nomes"). Everything else must go through i18n (`t("ns:key")`).
 * The first three patterns repeat the plugin defaults (numbers/punctuation, CONSTANT_CASE,
 * emoji), which are replaced, not merged, when `words.exclude` is set.
 */
export const I18N_LITERAL_ALLOWLIST = [
  "[0-9!-/:-@[-`{-~]+",
  "[A-Z_-]+",
  "^\\p{Emoji}+$",
  "^(Forgebox|forgeboxd|forgebox-helper|Forgebox\\.exe)$",
  "^forge( [a-z][a-z0-9:._-]*)*$",
  "^(PHP|Nginx|Apache|MySQL|MariaDB|PostgreSQL|Redis|Node\\.js|Composer|Laragon|Mailpit)$",
];

/** Flat-config block shared by `eslint.config.js` and the lint fixture test. */
/** @type {import("eslint").Linter.Config} */
export const i18nLiteralConfig = {
  ...i18next.configs["flat/recommended"],
  rules: {
    "i18next/no-literal-string": [
      "error",
      { mode: "jsx-only", words: { exclude: I18N_LITERAL_ALLOWLIST } },
    ],
  },
};
