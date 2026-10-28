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
