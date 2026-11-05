// @vitest-environment node
// The rule behind "every eslint-disable needs an inline reason" (M2-E4-T3), through the same
// flat-config block eslint.config.js uses.
import { ESLint } from "eslint";
import tseslint from "typescript-eslint";
import { describe, expect, it } from "vitest";
import { disableReasonConfig } from "../../eslint.disable.js";

const RULE = "forgebox-disable/needs-reason";

async function lint(code: string) {
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      { files: ["**/*.ts"], languageOptions: { parser: tseslint.parser } },
      disableReasonConfig,
    ],
  });
  const [result] = await eslint.lintText(code, { filePath: "src/x.ts" });
  // Only this rule: the fixtures disable rules that are not even enabled (unused-directive noise).
  return (result?.messages ?? []).map((m) => m.ruleId).filter((id) => id === RULE);
}

describe("forgebox-disable/needs-reason", () => {
  it.each([
    "// eslint-disable-next-line no-console\nconsole.log(1);",
    "/* eslint-disable no-console */\nconsole.log(1);",
    "console.log(1); // eslint-disable-line no-console",
    "// eslint-disable-next-line no-console --\nconsole.log(1);",
    "// eslint-disable-next-line no-console --   \nconsole.log(1);",
  ])("fails a directive without a reason: %s", async (code) => {
    expect(await lint(code)).toEqual([RULE]);
  });

  it.each([
    "// eslint-disable-next-line no-console -- debug aid of the QA harness\nconsole.log(1);",
    "/* eslint-disable no-console -- fixture prints on purpose */\nconsole.log(1);",
    "console.log(1); // eslint-disable-line no-console -- reason",
    "// an ordinary comment about eslint\nconst a = 1;",
  ])("accepts a directive with a reason or no directive: %s", async (code) => {
    expect(await lint(code)).toEqual([]);
  });
});
