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

  it.each([
    ["aria-label", `<button aria-label="Fechar janela" />`],
    ["title", `<button title="Copiar comando" />`],
    ["placeholder", `<input placeholder="Pesquisar sites" />`],
    ["expression literal", `<p>{"Salvar alteracoes"}</p>`],
    ["ternary literal", `<p>{ok ? "Pronto" : "Falhou"}</p>`],
  ])("fails on a user-visible literal: %s", async (_name, jsx) => {
    const messages = await lint(`export function Screen({ ok }: { ok: boolean }) { return ${jsx}; }`);
    expect(messages.map((m) => m.ruleId)).toContain("i18next/no-literal-string");
  });

  it("accepts className and data-* attributes with allowlisted words", async () => {
    const messages = await lint(
      `export function Screen() {
        return <div className="flex gap-2" data-testid="card" data-state="open"><b>Forgebox</b><code>forge up</code></div>;
      }`,
    );
    expect(messages).toEqual([]);
  });
});
