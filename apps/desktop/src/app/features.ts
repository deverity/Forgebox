// The feature catalogue of the shell: one row per screen with its route, the roadmap task that
// delivers it (named by the placeholder) and whether the sidebar lists it. The sidebar, the
// placeholders and the command palette (M2-E1-T14) read this table; a screen never edits the
// shell, it replaces its placeholder at `src/features/<id>/index.tsx`.

export const FEATURE_IDS = [
  "dashboard",
  "services",
  "sites",
  "php",
  "databases",
  "mail",
  "logs",
  "catalog",
  "terminal",
  "settings",
  "diagnostics",
  "onboarding",
] as const;

export type FeatureId = (typeof FEATURE_IDS)[number];

export type IconName =
  | "dashboard"
  | "services"
  | "sites"
  | "php"
  | "databases"
  | "mail"
  | "logs"
  | "catalog"
  | "terminal"
  | "settings";

export interface FeatureDef {
  id: FeatureId;
  /** Route path of the list/landing screen. */
  path: string;
  /** Roadmap task that delivers the real screen (shown by the placeholder). */
  task: string;
  /** Sidebar group: `main`, `footer` (below the separator) or `none` (not in the sidebar). */
  nav: "main" | "footer" | "none";
  icon: IconName | null;
}

export const FEATURES: readonly FeatureDef[] = [
  { id: "dashboard", path: "/dashboard", task: "M2-E2-T14", nav: "main", icon: "dashboard" },
  { id: "services", path: "/services", task: "M2-E2-T12", nav: "main", icon: "services" },
  { id: "sites", path: "/sites", task: "M2-E2-T15", nav: "main", icon: "sites" },
  { id: "php", path: "/php", task: "M2-E2-T17", nav: "main", icon: "php" },
  { id: "databases", path: "/databases", task: "M2-E3-T14", nav: "main", icon: "databases" },
  { id: "mail", path: "/mail", task: "M2-E3-T13", nav: "main", icon: "mail" },
  { id: "logs", path: "/logs", task: "M2-E3-T12", nav: "main", icon: "logs" },
  { id: "catalog", path: "/catalog", task: "M2-E2-T18", nav: "main", icon: "catalog" },
  { id: "terminal", path: "/terminal", task: "M2-E3-T15", nav: "main", icon: "terminal" },
  { id: "settings", path: "/settings", task: "M2-E3-T16", nav: "footer", icon: "settings" },
  // Reached from the palette, Settings > Advanced and the status bar chips, not the sidebar.
  { id: "diagnostics", path: "/diagnostics", task: "M2-E3-T19", nav: "none", icon: null },
  { id: "onboarding", path: "/onboarding", task: "M2-E2-T19", nav: "none", icon: null },
];

export function featureOf(id: FeatureId): FeatureDef {
  const def = FEATURES.find((f) => f.id === id);
  if (def === undefined) throw new Error(`unknown feature ${id}`);
  return def;
}

/** The feature owning `pathname` (`/sites/abc` belongs to `sites`), or `null`. */
export function featureAt(pathname: string): FeatureDef | null {
  return FEATURES.find((f) => pathname === f.path || pathname.startsWith(`${f.path}/`)) ?? null;
}
