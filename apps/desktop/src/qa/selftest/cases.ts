// The qa:check self-test cases (M2-E4-T3). `/qa/selftest?case=<id>` renders one page; `ok` is
// clean and every other case breaks exactly one rule of docs/07 section 11, so each check in
// qa/checks/*.spec.ts is proven to pass a good page and to fail its negative one.
export const SELFTEST_CASES = [
  "ok",
  "axe",
  "sidebar-contrast",
  "accent-contrast",
  "unreachable",
  "no-focus-ring",
  "escape",
  "shortcut",
  "layout-shift",
  "motion",
  "contract-cli",
  "contract-error",
  "contract-local-only",
  "contract-client-path",
  "eternal-skeleton",
] as const;

export type SelftestCase = (typeof SELFTEST_CASES)[number];

export const SELFTEST_PATH = "/qa/selftest";

export function parseCase(search: string): SelftestCase {
  const value = new URLSearchParams(search).get("case");
  return SELFTEST_CASES.find((c) => c === value) ?? "ok";
}
