// The screenshot matrix of `pnpm qa:shots` (M2-E4-T2, docs/07 section 12). Pure data and naming:
// no React, no Playwright, so vitest can check it against the router (src/qa/matrix.test.ts).
// Routes come from the router; a route without an entry in SCREENS (or EXCLUDED_ROUTES, with a
// reason) fails that test.

export const THEMES = ["light", "dark"] as const;
export const CORE_DPRS = [1, 1.25, 1.5] as const;
export type Theme = (typeof THEMES)[number];
export type Locale = "pt-BR" | "en-US" | "pseudo";
export type Scenario = "empty" | "populated";

export interface Size {
  w: number;
  h: number;
}

/** Window sizes in DIPs; the last two are the work-area cases (docs/07 section 4). */
export const SIZE_MAIN: Size = { w: 1280, h: 800 };
export const SIZES_EXTRA: readonly Size[] = [
  { w: 960, h: 600 },
  { w: 1099, h: 700 },
  { w: 960, h: 540 },
  { w: 1093, h: 574 },
];

export interface ScreenDef {
  /** File-system friendly id; the first path segment of every PNG. */
  id: string;
  /** The router's route pattern (the single source: checked against the router). */
  route: string;
  /** Concrete URL path (`$id` replaced). */
  path: string;
  scenario: Scenario;
}

const SAMPLE_ID = "demo";

export const SCREENS: readonly ScreenDef[] = [
  { id: "onboarding", route: "/onboarding", path: "/onboarding", scenario: "empty" },
  { id: "dashboard", route: "/dashboard", path: "/dashboard", scenario: "populated" },
  { id: "services", route: "/services", path: "/services", scenario: "populated" },
  { id: "sites", route: "/sites", path: "/sites", scenario: "populated" },
  {
    id: "site-detail",
    route: "/sites/$id",
    path: `/sites/${SAMPLE_ID}`,
    scenario: "populated",
  },
  { id: "php", route: "/php", path: "/php", scenario: "populated" },
  { id: "databases", route: "/databases", path: "/databases", scenario: "populated" },
  { id: "mail", route: "/mail", path: "/mail", scenario: "populated" },
  { id: "logs", route: "/logs", path: "/logs", scenario: "populated" },
  { id: "catalog", route: "/catalog", path: "/catalog", scenario: "populated" },
  {
    id: "catalog-detail",
    route: "/catalog/$id",
    path: "/catalog/php",
    scenario: "populated",
  },
  { id: "terminal", route: "/terminal", path: "/terminal", scenario: "populated" },
  { id: "settings", route: "/settings", path: "/settings", scenario: "populated" },
  { id: "diagnostics", route: "/diagnostics", path: "/diagnostics", scenario: "populated" },
];

/** Router routes that are not screens, each with the reason. */
export const EXCLUDED_ROUTES: Readonly<Record<string, string>> = {
  "/": "redirects to /dashboard",
  "/__gallery": "component gallery, covered by its own e2e (dev and qa builds only)",
  "/qa/selftest": "negative cases of qa:check, not a screen",
};

/** Routes of `routes` that neither SCREENS nor EXCLUDED_ROUTES accounts for. */
export function missingRoutes(
  routes: readonly string[],
  screens: readonly ScreenDef[] = SCREENS,
): string[] {
  const known = new Set([...screens.map((s) => s.route), ...Object.keys(EXCLUDED_ROUTES)]);
  return routes.filter((route) => !known.has(route));
}

export interface Cell {
  screen: ScreenDef;
  theme: Theme;
  /** Windows "contrast themes": emulates `forced-colors: active` (informative). */
  forced: boolean;
  dpr: number;
  vibrancy: "on" | "off";
  motion: "full" | "reduced";
  locale: Locale;
  size: Size;
  /** Informative cells are shot but not part of the acceptance matrix. */
  informative: boolean;
}

const BASE = {
  theme: "light",
  forced: false,
  dpr: 1,
  vibrancy: "on",
  motion: "full",
  locale: "pt-BR",
  size: SIZE_MAIN,
  informative: false,
} as const satisfies Omit<Cell, "screen">;

/** `<scenario>-<theme>-<dpr>-<vib>-<motion>-<locale>-<w>x<h>` (the file name without `.png`). */
export function cellName(cell: Cell): string {
  return [
    cell.screen.scenario,
    cell.forced ? "forced" : cell.theme,
    `${String(cell.dpr)}x`,
    `vib${cell.vibrancy}`,
    cell.motion,
    cell.locale,
    `${String(cell.size.w)}x${String(cell.size.h)}`,
  ].join("-");
}

/** Path relative to the run directory: `<screen>/<name>.png`. */
export function cellFile(cell: Cell): string {
  return `${cell.screen.id}/${cellName(cell)}.png`;
}

/** Expected PNG size in device pixels. */
export function pngSize(cell: Cell): Size {
  return { w: Math.round(cell.size.w * cell.dpr), h: Math.round(cell.size.h * cell.dpr) };
}

export type Subset = "full" | "ci";

/** Screens of the reduced CI subset. */
const CI_SCREENS = new Set(["dashboard", "services", "sites"]);

function dedupe(cells: Cell[]): Cell[] {
  const seen = new Set<string>();
  return cells.filter((cell) => {
    const key = cellFile(cell);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** Every cell of the matrix. `ci` is the reduced subset PRs run: core at DPR 1 + two extras. */
export function buildMatrix(
  subset: Subset = "full",
  screens: readonly ScreenDef[] = SCREENS,
): Cell[] {
  const cells: Cell[] = [];
  for (const screen of screens) {
    if (subset === "ci" && !CI_SCREENS.has(screen.id)) continue;
    const dprs = subset === "ci" ? ([1] as const) : CORE_DPRS;
    // Core: theme x DPR, pt-BR, vibrancy on.
    for (const theme of THEMES) {
      for (const dpr of dprs) cells.push({ ...BASE, screen, theme, dpr });
    }
    // Extras at DPR 1.
    cells.push({ ...BASE, screen, vibrancy: "off" });
    cells.push({ ...BASE, screen, motion: "reduced" });
    if (subset === "ci") continue;
    cells.push({ ...BASE, screen, locale: "en-US" });
    cells.push({ ...BASE, screen, locale: "pseudo" });
    cells.push({ ...BASE, screen, dpr: 2, informative: true });
    cells.push({ ...BASE, screen, forced: true, informative: true });
    // Window sizes (light and dark: the sidebar collapses below 1100 px).
    for (const size of SIZES_EXTRA) {
      for (const theme of THEMES) cells.push({ ...BASE, screen, theme, size });
    }
  }
  return dedupe(cells);
}

/** URL of a cell on the qa build. */
export function cellUrl(cell: Cell): string {
  const params = new URLSearchParams({
    scenario: cell.screen.scenario,
    theme: cell.theme,
    vibrancy: cell.vibrancy,
    motion: cell.motion,
    locale: cell.locale,
  });
  return `${cell.screen.path}?${params.toString()}`;
}
