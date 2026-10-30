// @vitest-environment node
// The no-direct-mutation rule (M2-E1-T13, E4-T5 parity gate), exercised through the same
// flat-config block `eslint.config.js` uses, on files inside and outside src/actions.
import { ESLint } from "eslint";
import tseslint from "typescript-eslint";
import { describe, expect, it } from "vitest";
import { actionsConfig, mutatingMethods } from "../../eslint.actions.js";
import { METHODS } from "../ipc/methods";

async function lint(code: string, filePath: string) {
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      { files: ["**/*.ts"], languageOptions: { parser: tseslint.parser } },
      actionsConfig,
    ],
  });
  const [result] = await eslint.lintText(code, { filePath });
  return result?.messages ?? [];
}

const BAD = `export async function f(api: { call(m: string, p: object): Promise<unknown> }) {
  await api.call("services.start", { targets: [] });
}`;

describe("forgebox/no-direct-mutation", () => {
  it("reads exactly the mutating methods of the generated table", () => {
    const expected = Object.entries(METHODS)
      .filter(([, meta]) => meta.mutating)
      .map(([name]) => name)
      .sort();
    expect([...mutatingMethods()].sort()).toEqual(expected);
    expect(expected.length).toBeGreaterThan(50);
  });

  it("flags a mutating call outside src/actions", async () => {
    const messages = await lint(BAD, "src/screens/sites.ts");
    expect(messages.map((m) => m.ruleId)).toEqual(["forgebox/no-direct-mutation"]);
    expect(messages[0]?.message).toContain("services.start");
  });

  it("flags a mutating job and a template literal", async () => {
    const messages = await lint(
      "export function f(q: { job(m: string): void }) { q.job(`net.dns_enable`); }",
      "src/screens/dns.ts",
    );
    expect(messages).toHaveLength(1);
  });

  it("accepts reads, and the same call inside src/actions", async () => {
    const read = `export function f(api: { call(m: string): void }) { api.call("services.status"); }`;
    expect(await lint(read, "src/screens/sites.ts")).toEqual([]);
    expect(await lint(BAD, "src/actions/services.ts")).toEqual([]);
  });
});
