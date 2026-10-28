import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { createFormatters } from "./format";
import { resolveLocale } from "./index";
import { pseudoize, pseudoizeBundle } from "./pseudo";

const root = dirname(fileURLToPath(import.meta.url));
const LOCALES = ["pt-BR", "en-US"] as const;

type Flat = Map<string, string>;

function flatten(value: unknown, prefix = "", out: Flat = new Map()): Flat {
  if (typeof value === "string") out.set(prefix, value);
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) flatten(v, prefix ? `${prefix}.${k}` : k, out);
  }
  return out;
}

/** CLDR plural categories differ per language (pt has `many`); compare the base key. */
const PLURAL = /_(zero|one|two|few|many|other)$/;
const baseKey = (key: string) => key.replace(PLURAL, "");
const placeholders = (text: string) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

function namespaces(locale: string): string[] {
  return readdirSync(join(root, locale))
    .filter((f) => f.endsWith(".json"))
    .map((f) => f.slice(0, -5))
    .sort();
}

function load(locale: string, ns: string): Flat {
  return flatten(JSON.parse(readFileSync(join(root, locale, `${ns}.json`), "utf8")));
}

/** base key -> set of placeholder signatures across its plural forms. */
function group(m: Flat) {
  const g = new Map<string, Set<string>>();
  for (const [k, v] of m) {
    const set = g.get(baseKey(k)) ?? new Set<string>();
    set.add(placeholders(v).join(","));
    g.set(baseKey(k), set);
  }
  return g;
}

describe("locale parity", () => {
  it("has the same namespace files in every locale", () => {
    expect(namespaces("pt-BR")).toEqual(namespaces("en-US"));
    expect(namespaces("en-US")).toEqual(
      expect.arrayContaining(["common", "shell", "errors", "tray", "notifications", "palette"]),
    );
  });

  // Generic over every namespace file, so new features are covered without editing this test.
  for (const ns of namespaces("en-US")) {
    it(`${ns}: same keys and identical {placeholders} in pt-BR and en-US`, () => {
      const ga = group(load("pt-BR", ns));
      const gb = group(load("en-US", ns));
      expect([...ga.keys()].sort()).toEqual([...gb.keys()].sort());
      for (const [key, phA] of ga) {
        expect([...phA].sort(), `${ns}:${key}`).toEqual([...(gb.get(key) ?? [])].sort());
      }
      for (const locale of LOCALES) {
        for (const [key, text] of load(locale, ns)) {
          expect(text.trim(), `${locale} ${ns}:${key} empty`).not.toBe("");
        }
      }
    });
  }
});

describe("formatters", () => {
  const MB = 1024 * 1024;
  const now = new Date("2026-10-28T12:00:00Z");
  const ago = (ms: number) => new Date(now.getTime() - ms);
  const pt = createFormatters("pt-BR");
  const en = createFormatters("en-US");

  it("formats bytes", () => {
    expect(pt.bytes(410 * MB)).toBe("410 MB");
    expect(en.bytes(410 * MB)).toBe("410 MB");
    expect(pt.bytes(1.5 * 1024 * MB)).toBe("1,5 GB");
    expect(en.bytes(1.5 * 1024 * MB)).toBe("1.5 GB");
    expect(en.bytes(0)).toBe("0 B");
  });

  it("formats durations", () => {
    const ms = (2 * 3600 + 14 * 60) * 1000;
    expect(pt.duration(ms)).toBe("2h 14m");
    expect(en.duration(ms)).toBe("2h 14m");
    expect(en.duration(45_000)).toBe("45s");
    expect(en.duration(350)).toBe("350ms");
    expect(en.duration(3 * 86_400_000 + 5 * 3_600_000)).toBe("3d 5h");
  });

  it("formats relative time", () => {
    expect(pt.relative(ago(2 * 60_000), now)).toBe("há 2 min");
    expect(en.relative(ago(2 * 60_000), now)).toBe("2 min ago");
    expect(en.relative(new Date(now.getTime() + 5 * 60_000), now)).toBe("in 5 min");
  });

  it("formats percent, numbers and dates by locale", () => {
    expect(pt.percent(0.255)).toBe("25,5%");
    expect(en.percent(0.255)).toBe("25.5%");
    expect(pt.number(1234.5)).toBe("1.234,5");
    expect(en.number(1234.5)).toBe("1,234.5");
    expect(en.date(now, { dateStyle: "short", timeZone: "UTC" })).toBe("10/28/26");
    expect(pt.date(now, { dateStyle: "short", timeZone: "UTC" })).toBe("28/10/2026");
  });
});

describe("locale resolution", () => {
  it("defaults to pt-BR and honours the override before the OS", () => {
    expect(resolveLocale(undefined, "fr-FR")).toBe("pt-BR");
    expect(resolveLocale("en", "pt-BR")).toBe("en-US");
    expect(resolveLocale(null, "en-GB")).toBe("en-US");
    expect(resolveLocale(null, "pt-PT")).toBe("pt-BR");
  });
});

describe("pseudo-locale", () => {
  it("accents, pads by at least 40 % and keeps placeholders", () => {
    const input = "Hello {name}";
    const out = pseudoize(input);
    expect(out).toContain("{name}");
    expect(out).toContain("Ĥéļļó");
    expect(out.length).toBeGreaterThanOrEqual(input.length * 1.4);
  });

  it("transforms nested bundles", () => {
    const out = pseudoizeBundle({ a: { b: "x {n}" } });
    expect(out.a.b).toContain("{n}");
    expect(out.a.b).not.toBe("x {n}");
  });
});
