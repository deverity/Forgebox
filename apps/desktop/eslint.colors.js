// Colours come from design tokens only (docs/07 sections 2.3 and 11): no colour literals in
// TS/TSX, no Tailwind arbitrary colours, no text below 11px.
const COLOR_FN = "(rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color)";
// esquery regexes cannot contain backslashes, so character classes stand in for \b, \w, \(.
const HEX = "(^|[^&0-9a-z_])#[0-9a-f]{3,8}([^0-9a-z_]|$)";
const FN = `(^|[^a-z0-9_-])${COLOR_FN}[(]`;
const SMALL_TEXT = "text-[[]([0-9]|10)([.][0-9]+)?(px|pt)."; // the last "." is the closing bracket

/** @param {string} pattern @param {string} message */
function literalRules(pattern, message) {
  return [
    { selector: `Literal[value=/${pattern}/i]`, message },
    { selector: `TemplateElement[value.raw=/${pattern}/i]`, message },
  ];
}

export const COLOR_LITERAL_MESSAGE =
  "No colour literals: use a design token (docs/design/tokens.json) via a Tailwind token class or var(--token).";

/** Flat-config block shared by `eslint.config.js` and the lint fixture test. */
/** @type {import("eslint").Linter.Config} */
export const colorLiteralConfig = {
  rules: {
    "no-restricted-syntax": [
      "error",
      ...literalRules(HEX, COLOR_LITERAL_MESSAGE),
      ...literalRules(FN, COLOR_LITERAL_MESSAGE),
      ...literalRules(SMALL_TEXT, "No text below 11px (docs/07 2.1): use a typography token."),
    ],
  },
};
