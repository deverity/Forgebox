// Pure rules of the Versions tab: which spec `forge use` gets, why a version cannot be
// removed and which history rows can be rolled back (docs/05 §4, docs/07 §5.3).
import type {
  InstanceDto,
  SiteUsageDto,
  ToolVersionDto,
  UpdateHistoryEntryDto,
} from "../../../ipc/generated";

/** `8.4.13` -> `8.4`; a version with fewer than three numeric parts is its own series. */
export function seriesPrefix(version: string): string {
  const parts = version.split(".");
  return parts.length >= 3 ? parts.slice(0, 2).join(".") : version;
}

/**
 * What `tools.use` takes for `version`: the series (`forge use php 8.4`) when it is the only
 * installed version of that series, else the exact version, so the choice is never ambiguous.
 */
export function cliSpecFor(version: string, installed: readonly ToolVersionDto[]): string {
  const series = seriesPrefix(version);
  if (series === version) return version;
  const same = installed.filter((other) => seriesPrefix(other.version) === series);
  return same.length === 1 ? series : version;
}

/** Why a version is in use (it cannot be removed), most blocking first; `null` when free. */
export type InUse =
  { kind: "instance"; names: string[] } | { kind: "sites"; count: number } | { kind: "cli" };

export function inUse(
  version: ToolVersionDto,
  instances: readonly InstanceDto[],
  sites: readonly SiteUsageDto[],
): InUse | null {
  const owners = instances.filter((i) => i.version_id === version.id);
  if (owners.length > 0) {
    return { kind: "instance", names: owners.map((i) => `${i.tool}/${i.name}`) };
  }
  const users = sites.filter(
    (s) => s.version === version.version || version.version.startsWith(`${s.version}.`),
  );
  if (users.length > 0) return { kind: "sites", count: users.length };
  if (version.is_current) return { kind: "cli" };
  return null;
}

/** `php@8.3.36`: the spec `tools.remove` takes. */
export function exactSpec(version: ToolVersionDto): string {
  return `${version.tool}@${version.version}`;
}

/**
 * History rows that `tools.rollback` can undo: the newest completed, not yet undone update of
 * each series (a rollback restores the previous version of the series).
 */
export function rollbackCandidates(
  entries: readonly UpdateHistoryEntryDto[],
): ReadonlyMap<string, UpdateHistoryEntryDto> {
  const result = new Map<string, UpdateHistoryEntryDto>();
  // Entries run newest first.
  for (const entry of entries) {
    const series = entry.series;
    if (series === null || series === undefined || result.has(series)) continue;
    if (entry.rollback_of !== null && entry.rollback_of !== undefined) {
      // The newest row of the series is itself a rollback: nothing left to undo.
      result.set(series, entry);
      continue;
    }
    if (entry.status === "completed") result.set(series, entry);
  }
  for (const [series, entry] of result) {
    if (entry.rollback_of !== null && entry.rollback_of !== undefined) result.delete(series);
  }
  return result;
}
