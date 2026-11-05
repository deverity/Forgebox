// Pure rules of the catalog screens (docs/07 section 5.9): filters, release grouping, the
// variant choice and the install plan summary. Nothing here knows a tool.
import type {
  CatalogEntryDto,
  ToolPlanDto,
  ToolPlanItemDto,
  ToolReleaseDto,
  VariantDefDto,
} from "../../ipc/generated";

export type InstallFilter = "all" | "installed" | "updates";

export interface CatalogFilters {
  installed: InstallFilter;
  /** `null` = every category. */
  category: string | null;
}

export function matchesFilters(entry: CatalogEntryDto, filters: CatalogFilters): boolean {
  if (filters.category !== null && entry.category !== filters.category) return false;
  if (filters.installed === "installed" && !entry.installed) return false;
  if (filters.installed === "updates") {
    return entry.update_available !== undefined && entry.update_available !== null;
  }
  return true;
}

/** Distinct categories, sorted, empty ones left out. */
export function categoriesOf(entries: readonly CatalogEntryDto[]): string[] {
  const set = new Set<string>();
  for (const entry of entries) {
    if (entry.category !== null && entry.category !== "") set.add(entry.category);
  }
  return [...set].sort((a, b) => a.localeCompare(b));
}

/** Stacks first (large cards), everything else after. */
export function splitStacks(entries: readonly CatalogEntryDto[]): {
  stacks: CatalogEntryDto[];
  tools: CatalogEntryDto[];
} {
  const stacks: CatalogEntryDto[] = [];
  const tools: CatalogEntryDto[] = [];
  for (const entry of entries) (entry.kind === "stack" ? stacks : tools).push(entry);
  return { stacks, tools };
}

/** The newest version known for an entry (`latest`, else the first of `versions`). */
export function latestOf(entry: CatalogEntryDto): string | null {
  return entry.latest ?? entry.versions[0] ?? null;
}

export interface SeriesGroup {
  series: string;
  /** Newest first, as the daemon sends them. */
  releases: ToolReleaseDto[];
}

/** Groups releases by series keeping the order of first appearance (newest series first). */
export function groupBySeries(releases: readonly ToolReleaseDto[]): SeriesGroup[] {
  const groups = new Map<string, SeriesGroup>();
  for (const release of releases) {
    const group = groups.get(release.series);
    if (group === undefined) {
      groups.set(release.series, { series: release.series, releases: [release] });
    } else {
      group.releases.push(release);
    }
  }
  return [...groups.values()];
}

export interface VariantChoice {
  key: string;
  /** Values offered by the release, in the manifest's order of preference. */
  values: string[];
  default: string | null;
  labels: Record<string, string>;
}

/**
 * The variant dimension the user chooses (`ts: nts|ts`): the first manifest dimension that is
 * not derived by the provider and has more than one value among the builds of `release`.
 * `null` when there is nothing to choose.
 */
export function variantChoice(
  defs: readonly VariantDefDto[],
  release: ToolReleaseDto | undefined,
): VariantChoice | null {
  if (release === undefined) return null;
  const builds = release.variants ?? [];
  for (const def of defs) {
    if (def.derived === true) continue;
    const offered = new Set<string>();
    for (const build of builds) {
      const value = build.variant[def.key];
      if (value !== undefined) offered.add(value);
    }
    if (offered.size < 2) continue;
    const values = def.values.filter((value) => offered.has(value));
    for (const value of offered) if (!values.includes(value)) values.push(value);
    const preferred = def.default ?? null;
    return {
      key: def.key,
      values,
      default: preferred !== null && offered.has(preferred) ? preferred : (values[0] ?? null),
      labels: def.labels ?? {},
    };
  }
  return null;
}

/** `php@8.4.27`: the exact spec `tools.install` takes. */
export function installSpec(tool: string, version: string): string {
  return `${tool}@${version}`;
}

/** Newest release that is not installed yet; the newest one when all are. */
export function defaultRelease(releases: readonly ToolReleaseDto[]): ToolReleaseDto | undefined {
  return releases.find((release) => release.installed !== true) ?? releases[0];
}

export interface PlanSummary {
  /** Items that would be downloaded. */
  download: ToolPlanItemDto[];
  /** Items already in `bin/`. */
  present: ToolPlanItemDto[];
  totalBytes: number;
  /** Some size is a manifest estimate: show the approximate sign. */
  approximate: boolean;
  /** Items to download without any size (left out of the total). */
  unknownSize: number;
}

export function summarizePlan(plan: ToolPlanDto): PlanSummary {
  const download = plan.items.filter((item) => item.already_installed !== true);
  const present = plan.items.filter((item) => item.already_installed === true);
  return {
    download,
    present,
    totalBytes: plan.total_bytes,
    approximate: download.some((item) => item.size_estimated === true),
    unknownSize: plan.unknown_size ?? 0,
  };
}

/** The specs an install passes to `tools.install` (what the plan resolved). */
export function planSpecs(plan: ToolPlanDto): string[] {
  return plan.items.filter((item) => item.already_installed !== true).map((item) => item.spec);
}

/** Safe `http(s)` URL or `null` (manifest links are untrusted). */
export function safeHttpUrl(url: string | null | undefined): string | null {
  if (url === null || url === undefined) return null;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" || parsed.protocol === "http:" ? parsed.href : null;
  } catch {
    return null;
  }
}
