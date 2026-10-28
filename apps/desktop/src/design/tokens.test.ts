import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { ESLint } from "eslint";
import stylelint from "stylelint";
import tseslint from "typescript-eslint";
import { afterEach, describe, expect, it, vi } from "vitest";
import { colorLiteralConfig } from "../../eslint.colors.js";
import stylelintConfig from "../../stylelint.config.js";
import { presets, resolveMotion, type PresetName } from "./motion";
import { colors, motion, radius, space, typography } from "./tokens";

const root = resolve(import.meta.dirname, "../..");
const script = join(root, "scripts/gen-tokens.mjs");
const sourceJson = resolve(root, "../../docs/design/tokens.json");

// --- WCAG contrast -------------------------------------------------------------------------

type Rgba = readonly [number, number, number, number];

function parseHex(hex: string): Rgba {
  const h = hex.replace("#", "");
  const n = (i: number) => Number.parseInt(h.slice(i, i + 2), 16);
  return [n(0), n(2), n(4), h.length === 8 ? n(6) / 255 : 1];
}

function over(fg: Rgba, bg: Rgba): Rgba {
  const a = fg[3];
  return [fg[0] * a + bg[0] * (1 - a), fg[1] * a + bg[1] * (1 - a), fg[2] * a + bg[2] * (1 - a), 1];
}

function luminance([r, g, b]: Rgba): number {
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

function contrast(fgHex: string, bgHex: string): number {
  const bg = parseHex(bgHex);
  const fg = over(parseHex(fgHex), bg);
  const [hi, lo] = [luminance(fg), luminance(bg)].sort((a, b) => b - a) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const FG = ["fg-primary", "fg-secondary", "fg-tertiary"] as const;
// bg-canvas and bg-sidebar are transparent with vibrancy on; the contract is the solid fallback.
const BG = ["bg-canvas", "bg-sidebar", "bg-surface"] as const;
const UI = [
  "stroke-strong",
  "accent",
  "state-running",
  "state-starting",
  "state-stopped",
  "state-failed",
  "state-updating",
] as const;

describe("colour contrast (WCAG AA)", () => {
  for (const theme of ["light", "dark"] as const) {
    const set = colors[theme];
    for (const fg of FG) {
      for (const bg of BG) {
        it(`${theme}: ${fg} on ${bg} >= 4.5:1`, () => {
          expect(contrast(set[fg], set[bg])).toBeGreaterThanOrEqual(4.5);
        });
      }
    }
    for (const fg of ["fg-primary", "fg-secondary"] as const) {
      it(`${theme}: ${fg} on bg-surface-2 >= 4.5:1`, () => {
        expect(contrast(set[fg], set["bg-surface-2"])).toBeGreaterThanOrEqual(4.5);
      });
    }
    for (const fg of UI) {
      for (const bg of BG) {
        it(`${theme}: ${fg} on ${bg} >= 3:1`, () => {
          expect(contrast(set[fg], set[bg])).toBeGreaterThanOrEqual(3);
        });
      }
    }
  }

  it("high contrast keeps text at AAA (7:1) and UI colours at 3:1", () => {
    const set = colors.highContrast;
    for (const fg of FG) {
      for (const bg of BG) {
        expect(contrast(set[fg], set[bg])).toBeGreaterThanOrEqual(7);
      }
    }
    for (const fg of UI) {
      expect(contrast(set[fg], set["bg-canvas"])).toBeGreaterThanOrEqual(3);
    }
  });

  it("the decorative divider stays a low-alpha stroke", () => {
    for (const theme of ["light", "dark"] as const) {
      const alpha = parseHex(colors[theme].stroke)[3];
      expect(alpha).toBeGreaterThanOrEqual(0.08);
      expect(alpha).toBeLessThanOrEqual(0.12);
    }
  });
});

describe("scales", () => {
  it("has no text style below 11px", () => {
    for (const [name, style] of Object.entries(typography)) {
      expect(style.fontSize, name).toBeGreaterThanOrEqual(11);
      expect(style.lineHeight, name).toBeGreaterThanOrEqual(style.fontSize);
    }
  });

  it("matches docs/07 2.1 and 2.2", () => {
    expect(typography.body).toMatchObject({ fontSize: 13, lineHeight: 18, fontWeight: 400 });
    expect(typography.display).toMatchObject({ fontSize: 28, lineHeight: 34, fontWeight: 600 });
    expect(typography.mono.tnum).toBe(true);
    expect(Object.values(space)).toEqual([4, 8, 12, 16, 20, 24, 32, 40]);
    expect(radius).toEqual({ control: 6, card: 10, sheet: 12, window: 8 });
  });

  it("matches docs/07 3 motion values", () => {
    expect(motion.spring).toEqual({
      layout: { stiffness: 380, damping: 32 },
      popover: { stiffness: 260, damping: 26 },
      toggle: { stiffness: 500, damping: 40 },
    });
    expect(motion.duration).toEqual({ micro: 120, transition: 220, sheet: 280, reduced: 100 });
  });
});

describe("generated css", () => {
  const css = readFileSync(join(root, "src/design/tokens.css"), "utf8");

  it("switches the backgrounds with data-vibrancy and has a high-contrast override", () => {
    expect(css).toMatch(
      /\[data-vibrancy="on"\][^}]*--bg-canvas: transparent;[^}]*--bg-sidebar: transparent;/,
    );
    expect(css).toMatch(/\[data-vibrancy="off"\][^}]*--bg-canvas: var\(--bg-canvas-solid\)/);
    expect(css).toContain(':root[data-contrast="high"]');
    expect(css).toContain("@media (forced-colors: active)");
    expect(css).toContain("@theme inline");
  });

  it("never declares a font size below 11px", () => {
    for (const m of css.matchAll(/--text-[a-z0-9-]+: (\d+(?:\.\d+)?)px;/g)) {
      expect(Number(m[1])).toBeGreaterThanOrEqual(11);
    }
  });
});

describe("tokens:check drift detection", () => {
  const run = (env: Record<string, string> = {}) =>
    spawnSync(process.execPath, [script, "--check"], {
      cwd: root,
      env: { ...process.env, ...env },
      encoding: "utf8",
    });

  it("exits 0 on a clean tree", () => {
    const res = run();
    expect(res.stderr).toBe("");
    expect(res.status).toBe(0);
  });

  it("exits 1 after tokens.json is edited without regenerating", () => {
    const dir = mkdtempSync(join(tmpdir(), "forge-tokens-"));
    try {
      const edited = join(dir, "tokens.json");
      writeFileSync(edited, readFileSync(sourceJson, "utf8").replace("#F5F5F7", "#F5F5F8"));
      const res = run({ TOKENS_SOURCE: edited });
      expect(res.status).toBe(1);
      expect(res.stderr).toContain("tokens drift");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("motion presets", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function mockReducedMotion(matches: boolean) {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query.includes("prefers-reduced-motion") && matches,
      media: query,
    }));
  }

  it("resolves every preset to a 100 ms fade under prefers-reduced-motion", () => {
    mockReducedMotion(true);
    for (const name of Object.keys(presets) as PresetName[]) {
      const p = resolveMotion(name);
      expect(p.fadeOnly, name).toBe(true);
      expect(p.transition, name).toEqual({ type: "tween", duration: 0.1, ease: "linear" });
    }
  });

  it("keeps springs and durations otherwise", () => {
    mockReducedMotion(false);
    expect(resolveMotion("layout").transition).toEqual({
      type: "spring",
      stiffness: 380,
      damping: 32,
    });
    expect(resolveMotion("popover").transition).toEqual({
      type: "spring",
      stiffness: 260,
      damping: 26,
    });
    expect(resolveMotion("toggle").transition).toEqual({
      type: "spring",
      stiffness: 500,
      damping: 40,
    });
    expect(resolveMotion("micro").transition).toMatchObject({ type: "tween", duration: 0.12 });
    expect(resolveMotion("transition").transition).toMatchObject({ duration: 0.22 });
    expect(resolveMotion("sheet").transition).toMatchObject({ duration: 0.28 });
    expect(resolveMotion("sheet").fadeOnly).toBe(false);
  });
});

describe("colour-literal lint (eslint)", () => {
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ["**/*.{ts,tsx}"],
        languageOptions: {
          parser: tseslint.parser,
          parserOptions: { ecmaFeatures: { jsx: true } },
        },
      },
      colorLiteralConfig,
    ],
  });

  async function problems(code: string): Promise<number> {
    const [result] = await eslint.lintText(code, { filePath: "fixture.tsx" });
    return result?.messages.filter((m) => m.ruleId === "no-restricted-syntax").length ?? 0;
  }

  it.each([
    ['export const c = "#fff";'],
    ['export const c = "#FFFFFF80";'],
    ['export const c = "rgb(0 0 0)";'],
    ['export const c = "rgba(0,0,0,.5)";'],
    ['export const c = "hsl(10 20% 30%)";'],
    ['export const c = "oklch(0.7 0.1 200)";'],
    ["export const c = `color: #fff`;"],
    ['export const A = () => <div className="text-[#fff]" />;'],
    ['export const A = () => <div className="bg-[rgb(1,2,3)] p-2" />;'],
    ['export const A = () => <div className="text-[10px]" />;'],
    ['export const A = () => <div className="text-[9.5px]" />;'],
  ])("rejects %s", async (code) => {
    expect(await problems(code)).toBeGreaterThan(0);
  });

  it.each([
    ['export const A = () => <div className="bg-surface text-fg-primary text-body" />;'],
    ['export const A = () => <div className="text-[11px] p-[3px]" />;'],
    ['export const c = "var(--accent)";'],
    ['export const u = "https://example.test/page#section";'],
    ['export const id = "#root";'],
    ['export const s = "colour(s) are tokens";'],
  ])("accepts %s", async (code) => {
    expect(await problems(code)).toBe(0);
  });
});

describe("colour-literal lint (stylelint)", () => {
  async function warnings(code: string, codeFilename: string) {
    const result = await stylelint.lint({
      code,
      codeFilename: join(root, codeFilename),
      config: stylelintConfig,
      configBasedir: root,
    });
    return result.results[0]?.warnings.map((w) => w.rule) ?? [];
  }

  it("rejects hex and colour functions outside tokens.css", async () => {
    expect(await warnings("a { color: #fff; }", "src/x.css")).toContain("color-no-hex");
    expect(await warnings("a { color: rgb(0 0 0); }", "src/x.css")).toContain(
      "function-disallowed-list",
    );
    expect(await warnings("a { color: oklch(0.7 0.1 200); }", "src/x.css")).toContain(
      "function-disallowed-list",
    );
  });

  it("allows literals in tokens.css and var() elsewhere", async () => {
    expect(await warnings("a { color: #fff; }", "src/design/tokens.css")).toEqual([]);
    expect(await warnings("a { color: var(--fg-primary); }", "src/x.css")).toEqual([]);
  });

  it("rejects font sizes below 11px", async () => {
    const rule = "declaration-property-value-disallowed-list";
    expect(await warnings("a { font-size: 10px; }", "src/x.css")).toContain(rule);
    expect(await warnings("a { font-size: 9.5px; }", "src/x.css")).toContain(rule);
    expect(await warnings("a { font-size: 0.5rem; }", "src/x.css")).toContain(rule);
    expect(await warnings("a { font-size: 11px; }", "src/x.css")).toEqual([]);
    expect(await warnings("a { font-size: 13px; }", "src/x.css")).toEqual([]);
  });
});
