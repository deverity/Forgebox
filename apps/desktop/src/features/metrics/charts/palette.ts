// Chart colours come only from design tokens (docs/07 sections 2.3 and 11): they are read from
// the CSS custom properties of the chart's own element at draw time, so a theme, vibrancy or
// contrast switch just re-reads them (see useChartTheme). No colour literals live in TS.

export interface ChartPalette {
  grid: string;
  axis: string;
  text: string;
  surface: string;
  accent: string;
  warn: string;
  danger: string;
  info: string;
  ok: string;
  /** Font family of the host element, for canvas text. */
  fontFamily: string;
}

/** CSS system colour keyword: a valid canvas colour when a token is unavailable (tests). */
const FALLBACK = "CanvasText";

function token(style: CSSStyleDeclaration, name: string): string {
  const value = style.getPropertyValue(name).trim();
  return value === "" ? FALLBACK : value;
}

export function readPalette(el: Element): ChartPalette {
  const style = getComputedStyle(el);
  return {
    grid: token(style, "--stroke"),
    axis: token(style, "--fg-tertiary"),
    text: token(style, "--fg-secondary"),
    surface: token(style, "--bg-surface"),
    accent: token(style, "--accent"),
    warn: token(style, "--state-starting"),
    danger: token(style, "--state-failed"),
    info: token(style, "--state-updating"),
    ok: token(style, "--state-running"),
    fontFamily: style.fontFamily === "" ? "sans-serif" : style.fontFamily,
  };
}

/**
 * Tailwind token classes matching the palette entries, for the legend swatches and markers.
 * Series colours and swatches both come from the same tokens, so they re-theme together.
 */
export const SWATCH = {
  accent: "bg-accent",
  warn: "bg-state-starting",
  danger: "bg-state-failed",
  info: "bg-state-updating",
  ok: "bg-state-running",
} as const;

/**
 * Opacity of the latency bands (percent). 16 % left the dark theme's p99 band at 1.27:1 against
 * the surface; 20 % keeps every band at 1.3:1 or more in light and dark (palette.test.ts).
 */
export const BAND_ALPHA = 20;

/** Translucent version of a resolved colour, for band and area fills. */
export function withAlpha(color: string, percent: number): string {
  return `color-mix(in srgb, ${color} ${String(percent)}%, transparent)`;
}

/** Minimum text size on canvas (docs/07 section 2.1). */
export const AXIS_FONT_PX = 11;
