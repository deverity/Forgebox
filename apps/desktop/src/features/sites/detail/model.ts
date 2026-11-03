// Pure helpers of the Site detail (M2-E2-T16): PHP series and specs, the "version not
// installed" rejection of `sites.php`, the size of an install plan and the live state of a
// Procfile process. No React, no IPC.
import type { ForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";

/** `8.4` or `8.4.27` to the `php@…` spec `tools.install` and `tools.plan` take. */
export function phpSpec(version: string): string {
  return `php@${version}`;
}

/** The series (`8.4`) of a version or series. */
export function seriesOf(version: string): string {
  const [major, minor] = version.split(".");
  return minor === undefined ? version : `${major ?? ""}.${minor}`;
}

/** The series the site is on (what the dropdown shows): pinned series, else the resolved one. */
export function currentSeries(php: T.SitePhpDto | null): string | null {
  if (php === null) return null;
  if (php.series !== null) return php.series;
  const version = php.exact ?? php.resolved;
  return version === null ? null : seriesOf(version);
}

export interface PhpOption {
  value: string;
  installed: boolean;
}

/** Installed series plus the ones the providers offer; newest first. */
export function phpOptions(
  installed: readonly string[],
  available: readonly string[],
  current: string | null,
): PhpOption[] {
  const known = new Set(installed);
  const all = new Set([...installed, ...available]);
  if (current !== null) {
    all.add(current);
    known.add(current);
  }
  return [...all]
    .sort((a, b) => b.localeCompare(a, "en", { numeric: true }))
    .map((value) => ({ value, installed: known.has(value) }));
}

/**
 * `sites.php` refused because the version is not installed (ADR-0041): `not_found` with
 * `details {reason: "version_not_installed", args: {installed[], ...}}`.
 */
export function isNotInstalled(error: ForgeApiError): boolean {
  if (error.code !== "not_found") return false;
  return error.reason === "version_not_installed" || Array.isArray(error.args.installed);
}

export interface PlanSize {
  bytes: number;
  /** Any item has an estimated size, or an item has none: show "≈". */
  approximate: boolean;
  /** Everything is already installed. */
  nothing: boolean;
}

export function planSize(plan: T.ToolPlanDto): PlanSize {
  const toDownload = plan.items.filter((item) => item.already_installed !== true);
  return {
    bytes: plan.total_bytes,
    approximate:
      (plan.unknown_size ?? 0) > 0 || toDownload.some((item) => item.size_estimated === true),
    nothing: toDownload.length === 0,
  };
}

/** The process a `service.*` event is about (by instance, else by `<anything>/<name>`). */
export function processOfEvent(
  processes: readonly T.SiteProcessDto[],
  payload: T.ServicePayload,
): T.SiteProcessDto | undefined {
  return (
    processes.find((p) => p.instance_id != null && p.instance_id === payload.instance_id) ??
    processes.find((p) => payload.name === p.name || payload.name.endsWith(`/${p.name}`))
  );
}

export function isServicePayload(value: unknown): value is T.ServicePayload {
  return (
    typeof value === "object" &&
    value !== null &&
    "instance_id" in value &&
    "state" in value &&
    "name" in value
  );
}

export type CheckTone = "success" | "warning" | "danger" | "neutral";

export function checkTone(status: T.CheckStatus): CheckTone {
  switch (status) {
    case "ok":
      return "success";
    case "warn":
      return "warning";
    case "fail":
      return "danger";
    case "skipped":
      return "neutral";
  }
}

/** Days until `iso`; negative when past. */
export function daysUntil(iso: string, now: number): number {
  return Math.floor((Date.parse(iso) - now) / 86_400_000);
}
