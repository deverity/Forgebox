import { useCallback, useRef, useState } from "react";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type * as T from "../../ipc/generated";
import { useForgeApi } from "../../state";
import { groupOf, mergeRerun } from "./model";

export type DoctorState =
  | { phase: "idle" }
  | { phase: "running"; message: string | null; pct: number | null }
  | { phase: "done"; checks: T.CheckDto[]; at: number }
  | { phase: "failed"; error: ForgeApiError };

export interface Doctor {
  state: DoctorState;
  /** Groups being checked again after a fix (or a manual re-run). */
  rerunning: ReadonlySet<string>;
  run: () => void;
  /** `doctor {checks: [group]}` and merge into the current list. */
  rerun: (group: string) => Promise<void>;
}

/**
 * Runs `daemon.doctor` as a job and follows its progress (docs/07 §5.12). A fix re-runs only
 * the check's group (`{checks: ['dns']}`) and the fresh rows replace the old ones in place.
 */
export function useDoctor(): Doctor {
  const api = useForgeApi();
  const [state, setState] = useState<DoctorState>({ phase: "idle" });
  const [rerunning, setRerunning] = useState<ReadonlySet<string>>(new Set());
  // The run being followed; a late answer of an older run is ignored.
  const generation = useRef(0);

  const run = useCallback(() => {
    generation.current += 1;
    const mine = generation.current;
    setState({ phase: "running", message: null, pct: null });
    api
      .job(
        "daemon.doctor",
        {},
        {
          onProgress: (progress) => {
            if (generation.current !== mine) return;
            setState({
              phase: "running",
              message: progress.message ?? null,
              pct: progress.pct ?? null,
            });
          },
        },
      )
      .then((handle) => handle.done)
      .then(
        (report) => {
          if (generation.current !== mine) return;
          setState({ phase: "done", checks: report.checks, at: Date.now() });
        },
        (error: unknown) => {
          if (generation.current !== mine) return;
          setState({ phase: "failed", error: toForgeApiError(error, { method: "daemon.doctor" }) });
        },
      );
  }, [api]);

  const rerun = useCallback(
    async (group: string) => {
      setRerunning((current) => new Set(current).add(group));
      try {
        const handle = await api.job("daemon.doctor", { checks: [group] });
        const report = await handle.done;
        setState((current) =>
          current.phase === "done"
            ? {
                ...current,
                checks: mergeRerun(
                  current.checks,
                  report.checks.filter((check) => groupOf(check.id) === group),
                  group,
                ),
              }
            : current,
        );
      } finally {
        setRerunning((current) => {
          const next = new Set(current);
          next.delete(group);
          return next;
        });
      }
    },
    [api],
  );

  return { state, rerunning, run, rerun };
}
