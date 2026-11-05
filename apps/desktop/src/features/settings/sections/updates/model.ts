// Pure helpers of Settings > Atualizacoes (docs/05 section 4).
import type { UpdatePolicy } from "../../../../ipc/generated";
import { valueAt } from "../../model";

/** Policies of `forge tool policy`, from the most manual to the most automatic. */
export const UPDATE_POLICIES: readonly UpdatePolicy[] = [
  "manual",
  "notify",
  "auto-patch",
  "auto-minor",
  "auto",
];

export type QuietProblem = "format" | "incomplete" | "same";

const HHMM = /^([01]\d|2[0-3]):[0-5]\d$/;

export function isHhmm(value: string): boolean {
  return HHMM.test(value.trim());
}

/**
 * Validates the quiet-hours pair like `QuietHours` in forge-core: both `HH:MM` (24 h) or both
 * empty (no quiet hours). A window may wrap midnight, so only identical times are refused.
 */
export function quietProblem(start: string, end: string): QuietProblem | undefined {
  const a = start.trim();
  const b = end.trim();
  if (a === "" && b === "") return undefined;
  if (a === "" || b === "") return "incomplete";
  if (!isHhmm(a) || !isHhmm(b)) return "format";
  return a === b ? "same" : undefined;
}

/** The value `updates.quiet_hours` is saved with: `{start, end}` or `null` (off). */
export function quietValue(start: string, end: string): { start: string; end: string } | null {
  return start.trim() === "" ? null : { start: start.trim(), end: end.trim() };
}

/** Quiet hours of the configuration document, or empty strings. */
export function quietOf(values: unknown): { start: string; end: string } {
  const raw = valueAt(values, "updates.quiet_hours");
  if (typeof raw === "object" && raw !== null && !Array.isArray(raw)) {
    const { start, end } = raw as { start?: unknown; end?: unknown };
    return {
      start: typeof start === "string" ? start : "",
      end: typeof end === "string" ? end : "",
    };
  }
  return { start: "", end: "" };
}
