import js from "@eslint/js";
import { defineConfig, globalIgnores } from "eslint/config";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";
import { colorLiteralConfig } from "./eslint.colors.js";
import { i18nLiteralConfig } from "./eslint.i18n.js";

export default defineConfig([
  // Build output, dependencies, Rust target and generated IPC contracts (cargo xtask gen-ts).
  globalIgnores([
    "dist",
    "dist-qa",
    "qa/out",
    "test-results",
    "playwright-report",
    "node_modules",
    "src-tauri",
    "src/ipc/generated.ts",
    "src/ipc/methods.ts",
  ]),
  {
    files: ["**/*.{ts,tsx}"],
    extends: [
      js.configs.recommended,
      tseslint.configs.strictTypeChecked,
      tseslint.configs.stylisticTypeChecked,
      reactHooks.configs.flat.recommended,
      jsxA11y.flatConfigs.recommended,
    ],
    languageOptions: {
      globals: globals.browser,
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
  },
  {
    // No literal UI text in JSX (docs/07 section 9): strings come from src/i18n via t().
    ...i18nLiteralConfig,
    files: ["src/**/*.tsx"],
    ignores: ["src/**/*.test.tsx"],
  },
  {
    // Design tokens only (docs/07 section 11). tokens.ts is generated from tokens.json and
    // tests hold literal fixtures, so both are exempt.
    ...colorLiteralConfig,
    files: ["src/**/*.{ts,tsx}"],
    ignores: ["src/design/tokens.ts", "src/**/*.test.{ts,tsx}"],
  },
  {
    // Config files are plain JS, outside the TS project.
    files: ["**/*.js"],
    extends: [js.configs.recommended],
    languageOptions: { globals: globals.node },
  },
]);
