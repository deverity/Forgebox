// Runs a job action from a settings card and follows it (M2-E3-T17): the job id is known as soon
// as the daemon accepts it, so `JobStatus` can show progress while a UAC prompt waits.
import { useCallback, useEffect, useRef, useState } from "react";
import { startJobAction, type Action } from "../../../../actions/registry";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { MethodName } from "../../../../ipc/methods";
import { useRuntime } from "../../../../state";

export type JobPhase = "idle" | "running" | "done" | "failed";

export interface JobRun<I> {
  phase: JobPhase;
  /** Set once the daemon accepted the job (before that, `phase` is already `running`). */
  jobId: string | null;
  failure: ForgeApiError | null;
  /** Retries the last input. */
  retry: () => void;
  run: (input: I) => void;
  reset: () => void;
}

interface State<I> {
  phase: JobPhase;
  jobId: string | null;
  failure: ForgeApiError | null;
  input: { value: I } | null;
}

/**
 * Starts `action` as a job and tracks it. A failure (`needs_elevation` when the UAC prompt was
 * refused, for one) stays in `failure` until the next run; `onDone` receives the job result.
 */
export function useJobAction<M extends MethodName, I>(
  action: Action<M, I>,
  onDone?: (result: unknown) => void,
): JobRun<I> {
  const runtime = useRuntime();
  const [state, setState] = useState<State<I>>({
    phase: "idle",
    jobId: null,
    failure: null,
    input: null,
  });
  const alive = useRef(true);
  const done = useRef(onDone);
  useEffect(() => {
    done.current = onDone;
  });
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const run = useCallback(
    (input: I) => {
      setState({ phase: "running", jobId: null, failure: null, input: { value: input } });
      const fail = (error: unknown) => {
        if (!alive.current) return;
        setState((current) => ({
          ...current,
          phase: "failed",
          failure: toForgeApiError(error, { method: action.method }),
        }));
      };
      startJobAction(runtime.queue, action, input, { intent: newIntent() }).then((handle) => {
        if (alive.current) setState((current) => ({ ...current, jobId: handle.job.id }));
        handle.done.then((result) => {
          if (!alive.current) return;
          setState((current) => ({ ...current, phase: "done" }));
          done.current?.(result);
        }, fail);
      }, fail);
    },
    [runtime, action],
  );
  const lastInput = state.input;
  const retry = useCallback(() => {
    if (lastInput !== null) run(lastInput.value);
  }, [lastInput, run]);
  const reset = useCallback(() => {
    setState({ phase: "idle", jobId: null, failure: null, input: null });
  }, []);
  return { phase: state.phase, jobId: state.jobId, failure: state.failure, retry, run, reset };
}
