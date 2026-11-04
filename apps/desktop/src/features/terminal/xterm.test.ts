// The emulator's look (M2-E4-T10, docs/07 section 9): colours and font come from the design
// tokens, every ANSI colour is a token (so a literal black/white pair cannot vanish on one of
// the two surfaces), and xterm is told to keep AA contrast on whatever a program prints.
import { afterEach, describe, expect, it } from "vitest";
import { colors } from "../../design/tokens";
import { MIN_CONTRAST, monoFontFamily, themeFromTokens } from "./xterm";

const ANSI = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
] as const;

function applyTokens(theme: "light" | "dark") {
  const root = document.documentElement;
  for (const [name, value] of Object.entries(colors[theme]))
    root.style.setProperty(`--${name}`, value);
  root.style.setProperty("--font-mono", '"Cascadia Code", "JetBrains Mono", monospace');
}

function luminance(hex: string): number {
  const channel = (i: number) => {
    const c = Number.parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2);
}

afterEach(() => {
  document.documentElement.removeAttribute("style");
});

describe("xterm theme from tokens", () => {
  for (const mode of ["light", "dark"] as const) {
    it(`${mode}: the surface, the text and all 16 ANSI colours are tokens`, () => {
      applyTokens(mode);
      const theme = themeFromTokens();
      expect(theme.background?.toLowerCase()).toBe(colors[mode]["bg-surface"].toLowerCase());
      expect(theme.foreground?.toLowerCase()).toBe(colors[mode]["fg-primary"].toLowerCase());
      for (const name of ANSI) expect(theme[name], `${mode} ${name}`).toBeDefined();
    });

    it(`${mode}: ANSI black is darker than white and brightWhite is lighter than white`, () => {
      applyTokens(mode);
      const theme = themeFromTokens();
      const lum = (name: (typeof ANSI)[number]) => luminance(theme[name] ?? "");
      expect(lum("black")).toBeLessThan(lum("white"));
      expect(lum("brightBlack")).toBeLessThan(lum("white"));
      expect(lum("brightWhite")).toBeGreaterThan(lum("white"));
      expect(lum("black")).toBeLessThan(lum("brightBlack"));
    });
  }

  it("asks xterm to keep WCAG AA contrast on program output", () => {
    expect(MIN_CONTRAST).toBe(4.5);
  });

  it("uses the --font-mono token (Cascadia Code, then the bundled JetBrains Mono)", () => {
    applyTokens("light");
    expect(monoFontFamily()).toContain("JetBrains Mono");
  });
});
