// @vitest-environment node
// Every self-test case is exercised by a spec of qa/checks (a case nobody asserts on would let
// its rule rot silently), and the case parameter falls back to the good page.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { parseCase, SELFTEST_CASES } from "./cases";

const CHECKS = join(import.meta.dirname, "../../../qa/checks");
const SPECS = readdirSync(CHECKS)
  .filter((name) => name.endsWith(".spec.ts"))
  .map((name) => readFileSync(join(CHECKS, name), "utf8"))
  .join("\n");

describe("qa self-test cases", () => {
  it.each(SELFTEST_CASES)("%s is used by a qa/checks spec", (which) => {
    expect(SPECS).toContain(`"${which}"`);
  });

  it("falls back to the clean page", () => {
    expect(parseCase("")).toBe("ok");
    expect(parseCase("?case=nope")).toBe("ok");
    expect(parseCase("?case=axe")).toBe("axe");
  });
});
