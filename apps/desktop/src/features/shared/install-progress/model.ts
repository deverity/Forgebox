// Pure helpers of <InstallProgressList>: how a `JobProgress` becomes a bar.
import type { JobProgress } from "../../../ipc/generated";

/** Highest percentage shown while the job still runs: 100 is reserved for the check. */
const RUNNING_CAP = 99;

/**
 * The bar value (0..99) or `null` when the total is unknown (indeterminate bar). `pct` wins;
 * otherwise bytes done over a known, positive total.
 */
export function barValue(progress: JobProgress): number | null {
  const { pct, bytes_done: done, bytes_total: total } = progress;
  if (typeof pct === "number" && Number.isFinite(pct)) {
    return Math.min(RUNNING_CAP, Math.max(0, pct));
  }
  if (typeof done === "number" && typeof total === "number" && total > 0) {
    return Math.min(RUNNING_CAP, Math.max(0, (done / total) * 100));
  }
  return null;
}

/** Positive finite number or `undefined` (speed and ETA are only shown when meaningful). */
export function positive(value: number | null | undefined): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}
