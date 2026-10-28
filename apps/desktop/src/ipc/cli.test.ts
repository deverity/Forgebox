// The TypeScript renderer must match the Rust one on the shared golden file (ADR-0037).
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseCliTemplate, quoteArg, renderCli, renderTemplate } from "./cli";
import { METHODS, type MethodName } from "./methods";

interface GoldenEntry {
  method: string;
  params: unknown;
  expected: string | null;
  note?: string;
}

const here = dirname(fileURLToPath(import.meta.url));
const golden = JSON.parse(
  readFileSync(
    join(here, "../../../../crates/forge-ipc/tests/fixtures/cli_templates.json"),
    "utf8",
  ),
) as GoldenEntry[];

describe("renderCli (golden shared with forge_ipc::api::cli_template)", () => {
  it("has entries", () => {
    expect(golden.length).toBeGreaterThan(100);
  });

  it("matches every golden entry byte for byte", () => {
    const diffs = golden
      .map((entry) => ({
        entry,
        actual: renderCli(entry.method as MethodName, entry.params),
      }))
      .filter(({ entry, actual }) => actual !== entry.expected)
      .map(
        ({ entry, actual }) => `${entry.method} ${JSON.stringify(entry.params)}: ${String(actual)}`,
      );
    expect(diffs).toEqual([]);
  });

  it("parses every template of the method table", () => {
    for (const [method, meta] of Object.entries(METHODS)) {
      const template = meta.cli_template as string | null;
      if (template === null) continue;
      expect(() => parseCliTemplate(template), method).not.toThrow();
    }
  });

  it("is null for methods without a command", () => {
    expect(renderCli("system.ping", {})).toBeNull();
  });
});

describe("quoteArg", () => {
  it.each([
    ["C:\\www\\loja", "C:\\www\\loja"],
    ["php@8.3", "php@8.3"],
    ["D:\\meus sites\\", '"D:\\meus sites\\\\"'],
    ["@x", '"@x"'],
    ["a,b", '"a,b"'],
    ["", null],
    ["a$b", null],
    ["50%", null],
    ['say "hi"', null],
    ["tab\there", null],
  ])("%s → %s", (input, expected) => {
    expect(quoteArg(input)).toBe(expected);
  });

  it("makes values that look like flags unavailable", () => {
    expect(renderTemplate("forge x {a}", { a: "-rf" })).toBeNull();
  });

  it("rejects malformed templates instead of rendering them", () => {
    expect(renderTemplate("{a} forge", {})).toBeNull();
    expect(renderTemplate("forge {a", {})).toBeNull();
  });
});
