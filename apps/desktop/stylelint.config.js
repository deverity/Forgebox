// Colours come from tokens only (docs/07 section 11); tokens.css is the sole place for literals.
const COLOR_FUNCTIONS = ["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch"];

/** @type {import("stylelint").Config} */
export default {
  rules: {
    "color-no-hex": true,
    "function-disallowed-list": COLOR_FUNCTIONS,
    // No font size below 11px (docs/07 2.1); the rem pattern rejects anything under 0.68rem.
    "declaration-property-value-disallowed-list": {
      "font-size": [
        String.raw`/^([0-9]|10)(\.[0-9]+)?(px|pt)$/`,
        String.raw`/^0?\.([0-5][0-9]*|6[0-7][0-9]*)rem$/`,
      ],
    },
  },
  overrides: [
    {
      files: ["**/design/tokens.css"],
      rules: {
        "color-no-hex": null,
        "function-disallowed-list": null,
      },
    },
  ],
};
