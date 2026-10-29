import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// tokens.css resets --color-*, --text-*, --radius-* and --shadow-* in @theme, so Tailwind default
// palette/size utilities silently generate no CSS. This guard fails on them in source files.
const src = resolve(import.meta.dirname, "..");
const palette =
  "slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|white|black";
const banned: RegExp[] = [
  new RegExp(String.raw`(?<![\w-])(?:[a-z-]+:)*(?:bg|text|border|ring|fill|stroke|outline|divide|from|to|via)-(?:${palette})(?:-\d{2,3})?(?![\w-])`),
  /(?<![\w-])(?:[a-z-]+:)*text-(?:xs|sm|base|lg|[2-9]xl)(?![\w-])/,
  /(?<![\w-])(?:[a-z-]+:)*shadow-(?:sm|md|lg|xl|2xl)(?![\w-])/,
  /(?<![\w-])(?:[a-z-]+:)*rounded(?:-[a-z]+)?-(?:sm|md|lg|xl|2xl|3xl)(?![\w-])/,
];

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return files(p);
    return p.endsWith(".tsx") && !p.endsWith(".test.tsx") ? [p] : [];
  });
}

function offenders(text: string): string[] {
  const out: string[] = [];
  for (const m of text.matchAll(/(["'`])([^"'`]*)\1/g)) {
    for (const cls of (m[2] ?? "").split(/\s+/)) {
      if (banned.some((r) => r.test(cls))) out.push(cls);
    }
  }
  return out;
}

describe("tailwind default utilities are not used", () => {
  it("detects banned classes (self-check)", () => {
    expect(offenders(`<p className="bg-neutral-50 dark:text-white text-sm shadow-md rounded-lg">`)).toHaveLength(5);
    expect(offenders(`<p className="bg-canvas text-body text-fg-primary rounded-card">`)).toEqual([]);
  });
  it("src/**/*.tsx uses only token utilities", () => {
    const found = files(src).flatMap((f) => offenders(readFileSync(f, "utf8")).map((c) => `${f}: ${c}`));
    expect(found).toEqual([]);
  });
});

// Tailwind 4: `outline-hidden`/`outline-none` set --tw-outline-style: none, so
// `focus-visible:outline-2` alone draws nothing. A focus ring needs `focus-visible:outline-solid`
// (use FOCUS_RING from design/primitives/shared.ts).
const NO_RING_ALLOWLIST = [
  // TODO M2-E4-T6 / M2-E4-T8: T7 chrome controls still compute outline none on focus.
  "app/shell/Titlebar.tsx",
  "app/shell/WindowControls.tsx",
];

function invisibleFocusRings(text: string): string[] {
  const out: string[] = [];
  for (const m of text.matchAll(/(["'`])([^"'`]*)\1/g)) {
    const classes = (m[2] ?? "").split(/\s+/);
    const hides = classes.some((c) => c === "outline-hidden" || c === "outline-none");
    const ring = classes.includes("focus-visible:outline-2");
    if (hides && ring && !classes.includes("focus-visible:outline-solid")) out.push(m[2] ?? "");
  }
  return out;
}

describe("focus rings are visible", () => {
  it("detects an invisible ring (self-check)", () => {
    expect(invisibleFocusRings(`"outline-hidden focus-visible:outline-2 focus-visible:outline-accent"`)).toHaveLength(1);
    expect(invisibleFocusRings(`"outline-none focus-visible:outline-solid focus-visible:outline-2"`)).toEqual([]);
  });
  it("src/**/*.tsx never pairs outline-hidden/none with focus-visible:outline-2 alone", () => {
    const found = files(src)
      .filter((f) => !NO_RING_ALLOWLIST.some((a) => f.replaceAll("\\", "/").endsWith(a)))
      .flatMap((f) => invisibleFocusRings(readFileSync(f, "utf8")).map((c) => `${f}: ${c}`));
    expect(found).toEqual([]);
  });
  it("shell nav links and status chips use FOCUS_RING", () => {
    for (const f of ["app/shell/Sidebar.tsx", "app/shell/StatusBar.tsx", "app/hosts.tsx"]) {
      expect(readFileSync(join(src, f), "utf8")).toContain("${FOCUS_RING}");
    }
  });
});
