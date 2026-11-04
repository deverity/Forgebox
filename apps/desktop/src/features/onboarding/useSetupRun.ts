// Step 4 of the wizard: the install of the stack, then the PATH, DNS, CA and startup jobs, each
// with its own status, "Tentar de novo" and "Pular". A failure never stops the others: a
// refused UAC becomes "skipped" (with its CLI) and the run goes on (docs/07 section 5.1,
// docs/11 section 1). Mutations go through the registered actions.
import { useCallback, useEffect, useRef, useState } from "react";
import { runAction, startJobAction } from "../../actions/registry";
import { addToPath, enableDns, installCa } from "../../actions/diagnostics";
import { setConfig } from "../../actions/system";
import { installTools } from "../../actions/tools";
import { newIntent } from "../../ipc/api";
import { ForgeApiError, toForgeApiError } from "../../ipc/errors";
import type { CaStatusDto, DnsStatusDto, ToolPlanDto, UserPathDto } from "../../ipc/generated";
import { useRuntime } from "../../state";
import type { InstallProgressItem } from "../shared/install-progress";
import { EMPTY_STACK, isElevationRefused, planParams, type RunStep, type RunStepId } from "./model";
import { onboardingNative } from "./native";

export type RunPhase = "idle" | "running" | "finished";

export interface SetupResults {
  path?: UserPathDto;
  dns?: DnsStatusDto;
  ca?: CaStatusDto;
}

export interface SetupRun {
  phase: RunPhase;
  steps: readonly RunStep[];
  /** One row per install job, for <InstallProgressList>. */
  rows: readonly InstallProgressItem[];
  results: SetupResults;
  /** What the install step downloads (its specs), once planned. */
  specs: readonly string[];
  start: () => void;
  retry: (id: RunStepId) => void;
  skip: (id: RunStepId) => void;
}

export interface SetupRunInput {
  stack: string | null;
  versions: Readonly<Record<string, string>>;
  steps: readonly RunStepId[];
}

const METHOD_OF: Record<RunStepId, string> = {
  parity: "system.config_set",
  install: "tools.install",
  path: "shell.path_add",
  dns: "net.dns_enable",
  ca: "tls.ca_install",
  autostart: "autostart_set",
};

function initialSteps(ids: readonly RunStepId[]): RunStep[] {
  return ids.map((id) => ({ id, status: "pending", error: null, skip: null }));
}

export function useSetupRun(input: SetupRunInput): SetupRun {
  const runtime = useRuntime();
  const inputRef = useRef(input);
  useEffect(() => {
    inputRef.current = input;
  });
  const [phase, setPhase] = useState<RunPhase>("idle");
  const [steps, setSteps] = useState<RunStep[]>(() => initialSteps(input.steps));
  const [rows, setRows] = useState<InstallProgressItem[]>([]);
  const [results, setResults] = useState<SetupResults>({});
  const [specs, setSpecs] = useState<string[]>([]);
  // The source of truth between renders: `patch` writes it before it asks React to render.
  const stepsRef = useRef(steps);
  const alive = useRef(true);
  const failedRows = useRef(new Set<string>());
  const counter = useRef(0);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const patch = useCallback((id: RunStepId, change: Partial<RunStep>) => {
    if (!alive.current) return;
    const next = stepsRef.current.map((step) => (step.id === id ? { ...step, ...change } : step));
    stepsRef.current = next;
    setSteps(next);
  }, []);

  const install = useCallback(async () => {
    const { stack, versions } = inputRef.current;
    if (stack === null || stack === EMPTY_STACK) {
      patch("install", { skip: "nothing" });
      return;
    }
    const planned = await runtime.queue.job("tools.plan", planParams(stack, versions), {
      intent: newIntent(),
    });
    const plan: ToolPlanDto = await planned.done;
    const missing = plan.items.filter((item) => item.already_installed !== true);
    if (alive.current) {
      setSpecs(missing.map((item) => item.spec));
      // A retry shows only what is still being installed: finished rows stay, failed ones go.
      setRows((current) => current.filter((row) => !failedRows.current.has(row.key)));
    }
    failedRows.current.clear();
    if (missing.length === 0) {
      patch("install", { skip: "nothing" });
      return;
    }
    let first: unknown = null;
    // In plan order (dependencies first), one at a time: each item has its own row and cause.
    for (const item of missing) {
      let key = "";
      try {
        const handle = await startJobAction(
          runtime.queue,
          installTools,
          { specs: [item.spec] },
          { intent: newIntent() },
        );
        counter.current += 1;
        key = `${handle.job.id}-${String(counter.current)}`;
        const row = { key, jobId: handle.job.id, label: `${item.tool} ${item.version}` };
        if (alive.current) setRows((current) => [...current, row]);
        await handle.done;
      } catch (error) {
        if (key !== "") failedRows.current.add(key);
        first ??= error;
      }
    }
    if (first !== null) throw toForgeApiError(first, { method: "tools.install" });
  }, [runtime, patch]);

  const execute = useCallback(
    async (id: RunStepId) => {
      patch(id, { status: "running", error: null, skip: null });
      const intent = { intent: newIntent() };
      try {
        switch (id) {
          case "parity":
            await runAction(
              runtime.queue,
              setConfig,
              { key: "case_sensitive", value: false },
              intent,
            );
            break;
          case "install":
            await install();
            break;
          case "path": {
            const path = (await runAction(runtime.queue, addToPath, undefined, intent)) as
              UserPathDto | undefined;
            if (alive.current && path !== undefined) setResults((r) => ({ ...r, path }));
            break;
          }
          case "dns": {
            const dns = (await runAction(runtime.queue, enableDns, undefined, intent)) as
              DnsStatusDto | undefined;
            if (alive.current && dns !== undefined) setResults((r) => ({ ...r, dns }));
            break;
          }
          case "ca": {
            const ca = (await runAction(
              runtime.queue,
              installCa,
              { scope: "current_user" },
              intent,
            )) as CaStatusDto | undefined;
            if (alive.current && ca !== undefined) setResults((r) => ({ ...r, ca }));
            break;
          }
          case "autostart": {
            const status = await onboardingNative().setAutostart(true);
            if (!status.enabled) {
              throw new ForgeApiError({
                code: "unsupported_platform",
                message: status.reason ?? "the Windows startup entry could not be created",
              });
            }
            break;
          }
        }
        const skip = stepsRef.current.find((step) => step.id === id)?.skip ?? null;
        patch(id, { status: skip === null ? "done" : "skipped" });
      } catch (error) {
        const failure = toForgeApiError(error, { method: METHOD_OF[id] });
        if (isElevationRefused(failure)) {
          patch(id, { status: "skipped", error: failure, skip: "elevation" });
        } else {
          patch(id, { status: "failed", error: failure, skip: null });
        }
      }
    },
    [runtime, patch, install],
  );

  const settle = useCallback(() => {
    if (!alive.current) return;
    const busy = stepsRef.current.some(
      (step) => step.status === "pending" || step.status === "running",
    );
    setPhase(busy ? "running" : "finished");
  }, []);

  const start = useCallback(() => {
    if (phase !== "idle") return;
    const fresh = initialSteps(inputRef.current.steps);
    stepsRef.current = fresh;
    setSteps(fresh);
    setPhase("running");
    void (async () => {
      for (const { id } of fresh) {
        const current = stepsRef.current.find((step) => step.id === id);
        if (current?.status === "pending") await execute(id);
        if (!alive.current) return;
      }
      settle();
    })();
  }, [phase, execute, settle]);

  const retry = useCallback(
    (id: RunStepId) => {
      setPhase("running");
      void execute(id).then(settle);
    },
    [execute, settle],
  );

  const skip = useCallback(
    (id: RunStepId) => {
      patch(id, { status: "skipped", skip: "user" });
      settle();
    },
    [patch, settle],
  );

  return { phase, steps, rows, results, specs, start, retry, skip };
}
