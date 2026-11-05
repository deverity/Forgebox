// Pure view-model of the PHP screen (docs/07 section 5.5): the columns, the extension search
// index, the optimistic overlay of the toggles and the req/s reading. No React, no IPC.
import type { PhpExtDto, PhpVersionDto, PoolDto, ToolDto, XdebugMode } from "../../ipc/generated";
import type { PoolSample } from "../../state";
import { fold } from "../sites/model";

/** The tool every column belongs to. */
export const PHP_TOOL_ID = "php";
/** The Xdebug client port the daemon uses when none was stored. */
export const XDEBUG_DEFAULT_PORT = 9003;
/** The `xdebug.idekey` the registry ships; no API reads or writes it yet (docs/07 section 5.5). */
export const IDE_KEY_DEFAULT = "FORGEBOX";
export const XDEBUG_MODES: readonly XdebugMode[] = ["off", "on", "trigger"];

/** `8.4` -> `[8, 4]`; a malformed series sorts last. */
function seriesKey(series: string): [number, number] {
  const [major, minor] = series.split(".").map((part) => Number.parseInt(part, 10));
  return [major ?? Number.MAX_SAFE_INTEGER, minor ?? 0];
}

/** Columns left to right, oldest series first (7.4 · 8.2 · 8.3 · 8.4), one per series. */
export function seriesColumns(versions: readonly PhpVersionDto[]): PhpVersionDto[] {
  const bySeries = new Map<string, PhpVersionDto>();
  for (const version of versions) {
    const known = bySeries.get(version.series);
    // Several patch versions of a series: the column shows the one the CLI uses, else the newest.
    if (
      known === undefined ||
      version.is_current_cli ||
      (!known.is_current_cli && newer(version, known))
    ) {
      bySeries.set(version.series, version);
    }
  }
  return [...bySeries.values()].sort((a, b) => {
    const [am, an] = seriesKey(a.series);
    const [bm, bn] = seriesKey(b.series);
    return am - bm || an - bn;
  });
}

function newer(a: PhpVersionDto, b: PhpVersionDto): boolean {
  const pa = a.version.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const pb = b.version.split(".").map((part) => Number.parseInt(part, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    const delta = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (delta !== 0) return delta > 0;
  }
  return false;
}

/** Variant badges (`NTS`, `vs17`, `x64`) in the order the build is named. */
export function variantBadges(version: Pick<PhpVersionDto, "thread_safety" | "compiler" | "arch">) {
  return [version.thread_safety.toUpperCase(), version.compiler, version.arch].filter(
    (part): part is string => part !== null && part !== "",
  );
}

/**
 * Whether the column's version is what the CLI runs. The tools store follows
 * `tool.current_changed` live; `php.list` is the fallback while the store has no PHP yet.
 */
export function isCliVersion(
  version: Pick<PhpVersionDto, "version" | "is_current_cli">,
  tool: Pick<ToolDto, "current"> | undefined,
): boolean {
  const current = tool?.current;
  if (current === null || current === undefined) return version.is_current_cli;
  // `tool.current_changed` carries the folder name (`8.4.1-nts-vs17-x64`) until the refetch.
  return current === version.version || current.startsWith(`${version.version}-`);
}

/** A pool for the card: the live store entry wins over the one `php.list` carried. */
export function livePool(listed: PoolDto | null, live: PoolDto | undefined): PoolDto | null {
  return live ?? listed;
}

/** The latest req/s of a pool (`null` before two samples, or after a counter reset). */
export function latestRate(samples: readonly PoolSample[]): number | null {
  return samples[samples.length - 1]?.rps ?? null;
}

// --- extensions ----------------------------------------------------------------------------

export interface ExtensionIndexEntry {
  extension: PhpExtDto;
  /** Folded name: a keystroke only runs `includes`. */
  haystack: string;
}

/** Sorted by name, with one folded haystack each. */
export function indexExtensions(extensions: readonly PhpExtDto[]): ExtensionIndexEntry[] {
  return [...extensions]
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .map((extension) => ({ extension, haystack: fold(extension.name) }));
}

/** Every word of `query` must appear in the name. */
export function filterExtensions(
  entries: readonly ExtensionIndexEntry[],
  query: string,
): ExtensionIndexEntry[] {
  const words = fold(query)
    .split(/\s+/)
    .filter((word) => word !== "");
  if (words.length === 0) return entries as ExtensionIndexEntry[];
  return entries.filter((entry) => words.every((word) => entry.haystack.includes(word)));
}

/** The state a toggle shows: the optimistic value while its job runs, else the real one. */
export function shownEnabled(
  extension: Pick<PhpExtDto, "name" | "enabled">,
  pending: Readonly<Record<string, boolean>>,
): boolean {
  return pending[extension.name] ?? extension.enabled;
}

/** The series label of the target the quick settings take (`php@8.4`). */
export function seriesTarget(series: string): string {
  return `${PHP_TOOL_ID}@${series}`;
}

/** Port field text to the `port` param: blank keeps the stored one; invalid is `undefined`. */
export function parseXdebugPort(text: string): number | null | undefined {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  if (!/^\d{1,5}$/.test(trimmed)) return undefined;
  const port = Number(trimmed);
  return port >= 1 && port <= 65_535 ? port : undefined;
}
