// Handlers of the Catalog screens (M2-E2-T18): `tools.search`, `tools.icon`, a richer
// `tools.info`, `tools.releases`, `tools.plan` and `tools.install` over a small fixed catalog.
// Shared by the QA scenarios (Playwright) and the vitest suites.
//
// Fixed behaviours the specs rely on:
// - `shady` has a hostile icon (script, handler attributes) and a third-party registry;
// - `nodejs` answers `tools.releases` from a stale cache (`stale: true`);
// - `legacy` answers `tools.releases` with a network failure;
// - `redis` installs until `jobs.cancel` (the cancel flow);
// - `laravel` is a stack of php + mysql + redis (sizes of mysql are estimates);
// - icons and brand colours are the official registry's (`registryIcons.ts`, M2-E1-T24), for
//   every registry id even outside this catalog (Services and Dashboard ask `tools.icon`);
//   `redis` and `legacy` have none (monogram fallback).
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import type { QaData } from "./data";
import { REGISTRY_ICONS, registryIcon } from "./registryIcons";

const MB = 1024 * 1024;

/** The icon of `shady`: everything a hostile manifest could try. */
export const HOSTILE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" onload="window.__pwned=1"><script>window.__pwned=2</script><image href="x" onerror="window.__pwned=3"/><circle r="4"/></svg>';
/** Brand colour of the third-party `shady` (a fixture value, not a registry one). */
const SHADY_COLOR = "#6A3FD4";

/** Not a real digest: fixtures only need a stable id. */
function sha(id: string): string {
  return id.padEnd(64, "0");
}

interface Seed {
  id: string;
  name: string;
  category: string;
  summary: string;
  versions: string[];
  kind?: T.CatalogKind;
  official?: boolean;
  registry?: string;
  size_mb?: number;
  native?: boolean;
  update?: string;
  icon?: string;
  /** `metadata.icon_color`; defaults to the registry's. */
  color?: string;
  tags?: string[];
}

const SEEDS: Seed[] = [
  {
    id: "php",
    name: "PHP",
    category: "runtime",
    summary: "The PHP interpreter, in several versions side by side.",
    versions: ["8.4.27", "8.4.26", "8.3.40", "8.3.39"],
    size_mb: 31,
  },
  {
    id: "mysql",
    name: "MySQL",
    category: "database",
    summary: "The MySQL database server.",
    versions: ["8.4.3", "8.4.2"],
    size_mb: 190,
    update: "8.4.3",
  },
  {
    id: "redis",
    name: "Redis",
    category: "cache",
    summary: "In-memory data store and cache.",
    versions: ["7.4.1"],
    size_mb: 12,
  },
  {
    id: "mailpit",
    name: "Mailpit",
    category: "mail",
    summary: "Catches the mail your apps send while you develop.",
    versions: ["1.21.8"],
    size_mb: 18,
  },
  {
    id: "nodejs",
    name: "Node.js",
    category: "runtime",
    summary: "JavaScript runtime.",
    versions: ["22.11.0", "20.18.0"],
    size_mb: 30,
    native: true,
  },
  {
    id: "legacy",
    name: "Legacy Tool",
    category: "utility",
    summary: "A tool whose providers are unreachable.",
    versions: ["1.0.0"],
    size_mb: 5,
  },
  {
    id: "shady",
    name: "Shady Tool",
    category: "utility",
    summary: "Third-party tool with a hostile icon.",
    versions: ["0.1.0"],
    official: false,
    registry: "community",
    size_mb: 2,
    icon: HOSTILE_SVG,
    color: SHADY_COLOR,
  },
  {
    id: "laravel",
    name: "Laravel",
    category: "stack",
    summary: "PHP, MySQL and Redis ready for a Laravel app.",
    versions: ["1.0.0"],
    kind: "stack",
    size_mb: 233,
    tags: ["php", "laravel", "onboarding"],
  },
  {
    id: "wordpress",
    name: "WordPress",
    category: "stack",
    summary: "PHP and MySQL ready for WordPress.",
    versions: ["1.0.0"],
    kind: "stack",
    size_mb: 221,
    tags: ["php", "wordpress", "onboarding"],
  },
  {
    id: "nextjs",
    name: "Next.js",
    category: "stack",
    summary: "Node.js ready for a Next.js app behind a proxy.",
    versions: ["1.0.0"],
    kind: "stack",
    size_mb: 30,
    tags: ["node", "nextjs", "onboarding"],
  },
];

/** What `tools.icon` serves: the registry's icons plus the fixture-only ones (`shady`). */
const ICONS = new Map<string, { svg: string; color: string | null }>([
  ...REGISTRY_ICONS,
  ...SEEDS.flatMap((s) =>
    s.icon === undefined ? [] : [[s.id, { svg: s.icon, color: s.color ?? null }] as const],
  ),
]);

function entryOf(seed: Seed, installed: ReadonlySet<string>): T.CatalogEntryDto {
  return {
    id: seed.id,
    name: seed.name,
    category: seed.category,
    description: seed.summary,
    summary: seed.summary,
    tags: seed.tags ?? [],
    registry: seed.registry ?? "official",
    official: seed.official ?? true,
    native: seed.native ?? true,
    versions: seed.versions,
    latest: seed.versions[0] ?? null,
    installed: installed.has(seed.id),
    kind: seed.kind ?? "tool",
    icon_sha256: ICONS.has(seed.id) ? sha(seed.id) : null,
    icon_color: seed.color ?? registryIcon(seed.id)?.color ?? null,
    update_available: seed.update ?? null,
    size_estimate_bytes: (seed.size_mb ?? 0) * MB,
    platforms: ["windows-x64"],
  };
}

function releasesOf(tool: string, seed: Seed, installed: ReadonlySet<string>): T.ToolReleaseDto[] {
  return seed.versions.map((version) => {
    const series = version.split(".").slice(0, 2).join(".");
    const sizeBytes = (seed.size_mb ?? 10) * MB;
    const variants: T.ReleaseVariantDto[] =
      tool === "php"
        ? ["nts", "ts"].map((ts) => ({
            variant: { ts, compiler: "vs17", arch: "x64" },
            canonical_name: `${version}-${ts}-vs17-x64`,
            size_bytes: sizeBytes,
          }))
        : [
            {
              variant: { arch: "x64" },
              canonical_name: `${version}-x64`,
              size_bytes: sizeBytes,
            },
          ];
    return {
      version,
      series,
      channel: "stable",
      size_bytes: sizeBytes,
      installed: installed.has(`${tool}@${version}`),
      variants,
    };
  });
}

const DESCRIPTION = [
  "## About",
  "",
  "A **fast** tool, see the [documentation](https://example.com/docs).",
  "",
  "- first point",
  "- second point with `code`",
  "",
  '<img src=x onerror="window.__pwned=4"> <script>window.__pwned=5</script>',
].join("\n");

/** What each fixture stack installs; a `tool@series` spec of a plan replaces the stack's entry. */
const STACK_SPECS: Record<string, string[]> = {
  laravel: ["php@8.4.27", "mysql@8.4.3", "redis@7.4.1"],
  wordpress: ["php@8.4.27", "mysql@8.4.3"],
  nextjs: ["nodejs@22.11.0"],
};

function stackSpecs(stack: string, overrides: readonly string[]): string[] {
  const wanted = STACK_SPECS[stack] ?? [];
  const replaced = new Set(overrides.map((spec) => spec.split("@")[0]));
  return [...wanted.filter((spec) => !replaced.has(spec.split("@")[0])), ...overrides];
}

/**
 * The catalog handlers over `data`. `base` is the scenario's existing handlers: `tools.info`
 * extends their answer (installed versions, policy) with the catalog metadata.
 */
export function catalogHandlers(_data: QaData, base: FakeHandlers = {}): FakeHandlers {
  // `tool@version` installed through `tools.install` in this session.
  const installed = new Set<string>();
  const installedTools = () => new Set([...installed].map((spec) => spec.split("@")[0] ?? ""));
  const seedOf = (id: string): Seed => {
    const seed = SEEDS.find((candidate) => candidate.id === id);
    if (seed === undefined) fakeError("not_found", `tool ${id} not found`);
    return seed;
  };
  const sizeOf = (tool: string) => (seedOf(tool).size_mb ?? 10) * MB;

  const plan = (specs: readonly string[], stack: string | null | undefined): T.ToolPlanDto => {
    const wanted =
      stack === undefined || stack === null
        ? specs
        : stack in STACK_SPECS
          ? stackSpecs(stack, specs)
          : fakeError("not_found", `stack ${stack} not found`);
    const items: T.ToolPlanItemDto[] = wanted.map((spec) => {
      const [tool = "", wantedVersion = ""] = spec.split("@");
      // A series (`8.3`) resolves to the newest version of that series, like the daemon.
      const version =
        SEEDS.find((seed) => seed.id === tool)?.versions.find(
          (candidate) => candidate === wantedVersion || candidate.startsWith(`${wantedVersion}.`),
        ) ?? wantedVersion;
      const estimated = tool === "mysql";
      return {
        tool,
        kind: "tool",
        spec,
        version,
        size_bytes: sizeOf(tool),
        size_estimated: estimated,
        already_installed: installed.has(spec),
        reason: "requested",
      };
    });
    const download = items.filter((item) => item.already_installed !== true);
    return {
      items,
      total_bytes: download.reduce((sum, item) => sum + (item.size_bytes ?? 0), 0),
      unknown_size: 0,
    };
  };

  return {
    "tools.search": ({ text, category }) => {
      const needle = (text ?? "").trim().toLowerCase();
      const have = installedTools();
      return {
        entries: SEEDS.filter(
          (seed) =>
            (category === undefined || category === null || seed.category === category) &&
            (needle === "" ||
              seed.id.includes(needle) ||
              seed.name.toLowerCase().includes(needle) ||
              seed.summary.toLowerCase().includes(needle)),
        ).map((seed) => entryOf(seed, have)),
      };
    },
    "tools.icon": ({ tool }) => {
      const icon = ICONS.get(tool);
      if (icon === undefined) fakeError("not_found", `${tool} has no icon`);
      return { tool, svg: icon.svg, sha256: sha(tool), icon_color: icon.color };
    },
    "tools.info": (params, ctx) => {
      const seed = seedOf(params.tool);
      const fromBase = base["tools.info"];
      const basic = fromBase === undefined ? undefined : (fromBase(params, ctx) as T.ToolInfoDto);
      const tool: T.ToolDto = basic?.tool ?? {
        id: seed.id,
        name: seed.name,
        category: seed.category,
        description: seed.summary,
        installed: [],
        current: null,
        policy: "notify",
        latest: seed.versions[0] ?? null,
        update_available: null,
        has_service: false,
      };
      const info: T.ToolInfoDto = {
        tool: { ...tool, name: seed.name, category: seed.category, description: seed.summary },
        // Keep what the base scenario offers (the site detail's PHP switch reads it).
        available: basic?.available ?? [],
        used_by: basic?.used_by ?? [],
        kind: seed.kind ?? "tool",
        description_md: DESCRIPTION,
        homepage: "https://example.com",
        docs: "https://example.com/docs",
        changelog_url: "https://example.com/changelog",
        license: "MIT",
        variants:
          seed.id === "php"
            ? [
                {
                  key: "ts",
                  values: ["nts", "ts"],
                  default: "nts",
                  labels: { nts: "Non Thread Safe", ts: "Thread Safe" },
                },
                { key: "compiler", values: ["vs17"], derived: true },
              ]
            : [],
      };
      if (seed.kind === "stack") {
        info.stack = {
          tools: [
            { id: "php", series: "8.4", extensions: ["xdebug"] },
            { id: "mysql", series: "8.4", instance: "default" },
            { id: "redis", instance: "default" },
          ],
        };
      }
      return info;
    },
    "tools.releases": ({ tool }) => {
      const seed = seedOf(tool);
      if (tool === "legacy") {
        return fakeJob({
          outcome: {
            failed: { code: "network", message: "providers unreachable" },
          },
        });
      }
      const releases = releasesOf(tool, seed, installed);
      return fakeJob({
        outcome: {
          done:
            tool === "nodejs"
              ? { tool, releases, stale: true, fetched_at: "2026-10-30T12:00:00Z" }
              : { tool, releases, stale: false, fetched_at: "2026-11-03T12:00:00Z" },
        },
      });
    },
    "tools.plan": ({ specs, stack }) => fakeJob({ outcome: { done: plan(specs ?? [], stack) } }),
    "tools.install": ({ specs }) => {
      const hold = specs.some((spec) => spec.startsWith("redis"));
      if (!hold) for (const spec of specs) installed.add(spec);
      return fakeJob({
        stepMs: 150,
        steps: [
          {
            message: "downloading",
            pct: 25,
            bytes_done: 8 * MB,
            bytes_total: 32 * MB,
            speed_bps: 4 * MB,
            eta_secs: 6,
          },
          {
            message: "downloading",
            pct: 70,
            bytes_done: 22 * MB,
            bytes_total: 32 * MB,
            speed_bps: 5 * MB,
            eta_secs: 2,
          },
          { message: "extracting" },
        ],
        ...(hold ? { hold: true } : { outcome: { done: { installed: [] } } }),
      });
    },
  };
}
