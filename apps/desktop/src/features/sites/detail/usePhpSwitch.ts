// The live PHP switch of one site (docs/06 section 2.2, docs/07 section 5.4):
//   idle -> switching (job progress) -> done, only after `job.done` AND `site.php_changed`;
//   a failure restores the selection and keeps the cause;
//   `not_found` + `version_not_installed` -> confirm (size from `tools.plan`) -> `tools.install`
//   -> the same switch again; cancelling leaves the old PHP.
// One switch per site at a time: while one is in flight (or waits on the user) `request`
// ignores new targets, so a second `sites.php` is never sent.
import { useCallback, useEffect, useRef, useState } from "react";
import { runAction } from "../../../actions/registry";
import { installToolSpec, switchSitePhp } from "../../../actions/sites";
import { newIntent } from "../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";
import { useForgeApi, useRuntime } from "../../../state";
import { isNotInstalled, phpSpec } from "./model";
import { useSiteEvents } from "./useSiteEvents";

/** How long `job.done` waits for a `site.php_changed` the event stream has not shown yet. */
export const CHANGE_WAIT_MS = 2000;
/** How long the success state stays before the badge goes back to idle. */
export const DONE_VISIBLE_MS = 2500;

export type PlanState =
  | { status: "loading" }
  | { status: "ready"; plan: T.ToolPlanDto }
  | { status: "failed"; error: ForgeApiError };

export type PhpSwitchState =
  | { phase: "idle" }
  | { phase: "switching"; target: string; progress: T.JobProgress | null }
  | { phase: "confirm_install"; target: string; plan: PlanState }
  | { phase: "installing"; target: string; progress: T.JobProgress | null }
  | { phase: "done"; target: string }
  | { phase: "failed"; target: string; error: ForgeApiError };

export interface PhpSwitch {
  state: PhpSwitchState;
  /** The series the dropdown shows: the target while a switch is in flight, else `current`. */
  selected: (current: string | null) => string | null;
  /** True while a request runs or waits on the install confirmation. */
  busy: boolean;
  request: (target: string) => void;
  confirmInstall: () => void;
  cancelInstall: () => void;
  dismiss: () => void;
}

const IDLE: PhpSwitchState = { phase: "idle" };

export function usePhpSwitch(siteId: string): PhpSwitch {
  const api = useForgeApi();
  const runtime = useRuntime();
  const [state, setState] = useState<PhpSwitchState>(IDLE);
  // Refs, not state: two clicks in the same tick must not both pass the guard.
  const inFlight = useRef(false);
  const alive = useRef(true);
  const pendingTarget = useRef<string | null>(null);
  const change = useRef<{ to: string } | null>(null);
  const waiters = useRef<(() => void)[]>([]);
  const doneTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (doneTimer.current !== null) clearTimeout(doneTimer.current);
    };
  }, []);

  useSiteEvents(siteId, (event) => {
    if (event.kind !== "site.php_changed") return;
    change.current = { to: (event.payload as T.SitePhpChangedPayload).to };
    for (const wake of waiters.current.splice(0)) wake();
  });

  const set = useCallback((next: PhpSwitchState) => {
    if (alive.current) setState(next);
  }, []);

  const waitForChange = useCallback(async () => {
    if (change.current !== null) return;
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, CHANGE_WAIT_MS);
      waiters.current.push(() => {
        clearTimeout(timer);
        resolve();
      });
    });
  }, []);

  const planInstall = useCallback(
    async (target: string) => {
      const update = (plan: PlanState) => {
        if (!alive.current) return;
        setState((s) =>
          s.phase === "confirm_install" && s.target === target ? { ...s, plan } : s,
        );
      };
      try {
        const handle = await api.job("tools.plan", { specs: [phpSpec(target)] });
        update({ status: "ready", plan: await handle.done });
      } catch (thrown) {
        update({ status: "failed", error: toForgeApiError(thrown, { method: "tools.plan" }) });
      }
    },
    [api],
  );

  const switchTo = useCallback(
    async (target: string) => {
      change.current = null;
      set({ phase: "switching", target, progress: null });
      try {
        await runAction(
          runtime.queue,
          switchSitePhp,
          { site: siteId, php: target },
          {
            intent: newIntent(),
            onProgress: (progress) => {
              if (!alive.current) return;
              setState((s) => (s.phase === "switching" ? { ...s, progress } : s));
            },
          },
        );
        // `site.php_changed` goes out before `job.done`, on another stream: give it a moment.
        await waitForChange();
        set({ phase: "done", target });
        doneTimer.current = setTimeout(() => {
          doneTimer.current = null;
          if (alive.current) setState((s) => (s.phase === "done" ? IDLE : s));
        }, DONE_VISIBLE_MS);
        inFlight.current = false;
      } catch (thrown) {
        const error = toForgeApiError(thrown, { method: "sites.php" });
        if (isNotInstalled(error)) {
          pendingTarget.current = target;
          set({ phase: "confirm_install", target, plan: { status: "loading" } });
          void planInstall(target);
          return; // The guard stays: the user decides next.
        }
        set({ phase: "failed", target, error });
        inFlight.current = false;
      }
    },
    [runtime, siteId, set, waitForChange, planInstall],
  );

  const request = useCallback(
    (target: string) => {
      if (inFlight.current) return;
      inFlight.current = true;
      if (doneTimer.current !== null) clearTimeout(doneTimer.current);
      void switchTo(target);
    },
    [switchTo],
  );

  const confirmInstall = useCallback(() => {
    const target = pendingTarget.current;
    if (target === null) return;
    pendingTarget.current = null;
    set({ phase: "installing", target, progress: null });
    void (async () => {
      try {
        await runAction(runtime.queue, installToolSpec, phpSpec(target), {
          intent: newIntent(),
          onProgress: (progress) => {
            if (!alive.current) return;
            setState((s) => (s.phase === "installing" ? { ...s, progress } : s));
          },
        });
      } catch (thrown) {
        set({
          phase: "failed",
          target,
          error: toForgeApiError(thrown, { method: "tools.install" }),
        });
        inFlight.current = false;
        return;
      }
      await switchTo(target); // Installed: the same switch again.
    })();
  }, [runtime, set, switchTo]);

  const cancelInstall = useCallback(() => {
    pendingTarget.current = null;
    inFlight.current = false;
    setState((s) => (s.phase === "confirm_install" ? IDLE : s));
  }, []);

  const dismiss = useCallback(() => {
    setState((s) => (s.phase === "failed" ? IDLE : s));
  }, []);

  const selected = useCallback(
    (current: string | null) =>
      state.phase === "idle" || state.phase === "failed" ? current : state.target,
    [state],
  );

  return {
    state,
    selected,
    busy:
      state.phase === "switching" ||
      state.phase === "confirm_install" ||
      state.phase === "installing",
    request,
    confirmInstall,
    cancelInstall,
    dismiss,
  };
}
