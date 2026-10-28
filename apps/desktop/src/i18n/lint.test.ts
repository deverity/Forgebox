// @vitest-environment node
// Note: eslint-plugin-i18next skips `const UpperCase = ...` declarations, so fixtures use functions.
import { ESLint } from "eslint";
import tseslint from "typescript-eslint";
import { describe, expect, it } from "vitest";
import { i18nLiteralConfig } from "../../eslint.i18n.js";

async function lint(code: string) {
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ["**/*.tsx"],
        languageOptions: {
          parser: tseslint.parser,
          parserOptions: { ecmaFeatures: { jsx: true } },
        },
      },
      { ...i18nLiteralConfig, files: ["**/*.tsx"] },
    ],
  });
  const [result] = await eslint.lintText(code, { filePath: "fixture.tsx" });
  return result?.messages ?? [];
}

describe("no-literal-string lint", () => {
  it("fails on a JSX literal", async () => {
    const messages = await lint(`export function Screen() { return <p>Salvar alteracoes</p>; }`);
    expect(messages.map((m) => m.ruleId)).toContain("i18next/no-literal-string");
  });

  it("accepts t() calls, brand names and forge commands", async () => {
    const messages = await lint(
      `export function Screen({ t }: { t: (k: string) => string }) {
        return <div><p>{t("shell:title")}</p><b>Forgebox</b><code>forge up</code></div>;
      }`,
    );
    expect(messages).toEqual([]);
  });
});
