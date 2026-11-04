// Handlers of the Catalog screens (M2-E2-T18): `tools.search`, `tools.icon`, a richer
// `tools.info`, `tools.releases`, `tools.plan` and `tools.install` over a small fixed catalog.
// Shared by the QA scenarios (Playwright) and the vitest suites.
//
// Fixed behaviours the specs rely on:
// - `shady` has a hostile icon (script, handler attributes) and a third-party registry;
// - `nodejs` answers `tools.releases` from a stale cache (`stale: true`);
// - `legacy` answers `tools.releases` with a network failure;
// - `redis` installs until `jobs.cancel` (the cancel flow);
// - `laravel` is a stack of php + mysql + redis (sizes of mysql are estimates).
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import type { QaData } from "./data";

const MB = 1024 * 1024;

/** The icon of `shady`: everything a hostile manifest could try. */
export const HOSTILE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" onload="window.__pwned=1"><script>window.__pwned=2</script><image href="x" onerror="window.__pwned=3"/><circle r="4"/></svg>';
const PLAIN_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3"/></svg>';

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
}

const SEEDS: Seed[] = [
  {
    id: "php",
    name: "PHP",
    category: "runtime",
    summary: "The PHP interpreter, in several versions side by side.",
    versions: ["8.4.27", "8.4.26", "8.3.40", "8.3.39"],
    size_mb: 31,
    icon: PLAIN_SVG,
  },
  {
    id: "mysql",
    name: "MySQL",
    category: "database",
    summary: "The MySQL database server.",
    versions: ["8.4.3", "8.4.2"],
    size_mb: 190,
    update: "8.4.3",
    icon: PLAIN_SVG,
  },
  {
    id: "redis",
    name: "Redis",
    category: "cache",
    summary: "In-memory data store and cache.",
    versions: ["7.4.1"],
    size_mb: 12,
    icon: PLAIN_SVG,
  },
  {
    id: "mailpit",
    name: "Mailpit",
    category: "mail",
    summary: "Catches the mail your apps send while you develop.",
    versions: ["1.21.8"],
    size_mb: 18,
    icon: PLAIN_SVG,
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
  },
  {
    id: "laravel",
    name: "Laravel",
    category: "stack",
    summary: "PHP, MySQL and Redis ready for a Laravel app.",
    versions: ["1.0.0"],
    kind: "stack",
    size_mb: 233,
  },
];

const ICONS = new Map(SEEDS.filter((s) => s.icon !== undefined).map((s) => [s.id, s.icon ?? ""]));

function entryOf(seed: Seed, installed: ReadonlySet<string>): T.CatalogEntryDto {
  return {
    id: seed.id,
    name: seed.name,
    category: seed.category,
    description: seed.summary,
    summary: seed.summary,
    tags: [],
    registry: seed.registry ?? "official",
    official: seed.official ?? true,
    native: seed.native ?? true,
    versions: seed.versions,
    latest: seed.versions[0] ?? null,
    installed: installed.has(seed.id),
    kind: seed.kind ?? "tool",
    icon_sha256: seed.icon === undefined ? null : sha(seed.id),
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
        : stack === "laravel"
          ? ["php@8.4.27", "mysql@8.4.3", "redis@7.4.1"]
          : fakeError("not_found", `stack ${stack} not found`);
    const items: T.ToolPlanItemDto[] = wanted.map((spec) => {
      const [tool = "", version = ""] = spec.split("@");
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
      const svg = ICONS.get(tool);
      if (svg === undefined) fakeError("not_found", `${tool} has no icon`);
      return { tool, svg, sha256: sha(tool) };
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
