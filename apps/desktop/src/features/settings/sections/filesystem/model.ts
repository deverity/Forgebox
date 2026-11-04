// Pure helpers of Settings > Sistema de arquivos (docs/15 sections 3 and 6).
import type { CaseFixChoice, CaseIssueDto, CaseSensitivityDto } from "../../../../ipc/generated";

/** Folders the Forgebox creates (and flags case-sensitive); `bin/` stays insensitive. */
export const CASE_DIRS = ["www", "etc", "data", "logs", "tmp"] as const;
export type CaseDirName = (typeof CASE_DIRS)[number];

/** Strategies of the Case Mismatch Finder, in display order. */
export const CASE_STRATEGIES: readonly CaseFixChoice[] = ["auto", "rename", "edit"];

export type CaseState = "enabled" | "disabled" | "unsupported";

/** `root/name` with `/` separators (the daemon takes either on Windows). */
export function joinPath(root: string, name: string): string {
  const base = root.replace(/[\\/]+$/, "").replaceAll("\\", "/");
  return `${base}/${name}`;
}

export function caseState(status: CaseSensitivityDto): CaseState {
  if (!status.supported) return "unsupported";
  return status.enabled === true ? "enabled" : "disabled";
}

/** Absolute path of a finding's file (the report names it relative to the project folder). */
export function issuePath(reportRoot: string, file: string): string {
  return joinPath(reportRoot, file.replace(/^[\\/]+/, ""));
}

/** Findings the finder can fix (a report-only category has no strategy). */
export function fixableCount(issues: readonly CaseIssueDto[]): number {
  return issues.filter((issue) => issue.fixes.length > 0).length;
}

/** A revert that stopped because two names differ only by case (`conflict` from the daemon). */
export function isRevertConflict(code: string | undefined, revert: boolean): boolean {
  return revert && code === "conflict";
}
