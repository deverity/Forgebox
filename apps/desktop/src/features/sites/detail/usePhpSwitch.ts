// The live PHP switch of one site (docs/06 section 2.2, docs/07 section 5.4):
//   idle -> switching (job progress) -> done, only after `job.done` AND the matching
//   `site.php_changed` (or a refetch that shows the new version);
//   a failure restores the selection and keeps the cause;
//   `not_found` + `version_not_installed` -> confirm (size from `tools.plan`) -> `tools.install`
//   -> the same switch again; cancelling leaves the old PHP.
// One switch per site at a time. The state lives in a controller keyed by (runtime, site), NOT
// in the component: the General tab unmounts when another tab or screen opens (Radix Tabs
// without forceMount), and a remount must still see the switch in flight, keep the dropdown
// locked and show the outcome. `request` ignores new targets while one is in flight, so a
// second `sites.php` is never sent.
import { useCallback, useMemo, useSyncExternalStore } from "react";
import { runAction } from "../../../actions/registry";
import { installToolSpec, switchSitePhp } from "../../../actions/sites";
import type { ForgeApi } from "../../../ipc/api";
import { newIntent } from "../../../ipc/api";
import { apiError, ForgeApiError, toForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";
import { useRuntime, type ContextRuntime } from "../../../state";
import { isNotInstalled, phpSpec } from "./model";

/** How long `job.done` waits for the `site.php_changed` of the new version before refetching. */
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
  /** `resolvedBefore`: the version serving the site now (tells whether an event is due). */
  request: (target: string, resolvedBefore?: string | null) => void;
  confirmInstall: () => void;
  cancelInstall: () => void;
  dismiss: () => void;
}

const IDLE: PhpSwitchState = { phase: "idle" };

const KINDS = ["site.php_changed"];

/** Non-React owner of one site's switch; it outlives the components that show it. */
class SwitchController {
  private state: PhpSwitchState = IDLE;
  private readonly listeners = new Set<() => void>();
  private inFlight = false;
  private pendingTarget: string | null = null;
  private resolvedBefore: string | null = null;
  /** `to` of every `site.php_changed` seen since the current switch started. */
  private changes: string[] = [];
  private waiters: (() => void)[] = [];
  private doneTimer: ReturnType<typeof setTimeout> | null = null;
  private unsubscribe: (() => Promise<void>) | null = null;

  constructor(
    private readonly api: ForgeApi,
    private readonly runtime: ContextRuntime,
    private readonly siteId: string,
  ) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): PhpSwitchState => this.state;

  private set(next: PhpSwitchState): void {
    this.state = next;
    for (const listener of [...this.listeners]) listener();
  }

  private update(fn: (s: PhpSwitchState) => PhpSwitchState): void {
    const next = fn(this.state);
    if (next !== this.state) this.set(next);
  }

  /** Listens to `site.php_changed` of this site for the length of one switch. */
  private async listen(): Promise<void> {
    await this.stopListening();
    try {
      const handle = await this.api.subscribe(
        "events.subscribe",
        { filter: { kinds: KINDS }, resume_from: null },
        (entries) => {
          for (const entry of entries) {
            if (entry.kind !== "item") continue;
            const event = entry.item.event;
            if (event.kind !== "site.php_changed") continue;
            const payload = event.payload as T.SitePhpChangedPayload;
            if (payload.site_id !== this.siteId) continue;
            this.changes.push(payload.to);
            for (const wake of this.waiters.splice(0)) wake();
          }
        },
      );
      this.unsubscribe = () => handle.unsubscribe();
    } catch {
      // Best effort: the refetch after the wait still confirms the version.
    }
  }

  private async stopListening(): Promise<void> {
    const off = this.unsubscribe;
    this.unsubscribe = null;
    await off?.().catch(() => undefined);
  }

  /** True when a `site.php_changed` to `version` was seen within the wait, false on timeout. */
  private async waitForChange(version: string): Promise<boolean> {
    const deadline = Date.now() + CHANGE_WAIT_MS;
    while (!this.changes.includes(version)) {
      const left = deadline - Date.now();
      if (left <= 0) return false;
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, left);
        this.waiters.push(() => {
          clearTimeout(timer);
          resolve();
        });
      });
    }
    return true;
  }

  /** Refetches the site: is the new version really serving? */
  private async confirmByRefetch(version: string): Promise<boolean> {
    try {
      const list = await this.api.call("sites.list", {});
      const site = list.sites.find((s) => s.id === this.siteId);
      return site?.php?.resolved === version;
    } catch {
      return false;
    }
  }

  private finish(next: PhpSwitchState): void {
    this.inFlight = false;
    void this.stopListening();
    this.set(next);
  }

  private async switchTo(target: string): Promise<void> {
    this.changes = [];
    this.set({ phase: "switching", target, progress: null });
    await this.listen();
    try {
      const result = await runAction(
        this.runtime.queue,
        switchSitePhp,
        { site: this.siteId, php: target },
        {
          intent: newIntent(),
          onProgress: (progress) => {
            this.update((s) => (s.phase === "switching" ? { ...s, progress } : s));
          },
        },
      );
      const after = (result as T.SiteDto | null)?.php?.resolved ?? null;
      // `site.php_changed` is only emitted when the resolved version changes (docs/03 section
      // 2.2): an unchanged one is done at `job.done`; a changed one needs its event, or a
      // refetch that shows it. Success is never declared unverified.
      if (after !== null && after !== this.resolvedBefore) {
        const seen = await this.waitForChange(after);
        if (!seen && !(await this.confirmByRefetch(after))) {
          this.finish({
            phase: "failed",
            target,
            error: new ForgeApiError(
              apiError("internal", `PHP ${after} was not confirmed for the site`),
              { method: "sites.php" },
            ),
          });
          return;
        }
      }
      this.finish({ phase: "done", target });
      this.doneTimer = setTimeout(() => {
        this.doneTimer = null;
        this.update((s) => (s.phase === "done" ? IDLE : s));
      }, DONE_VISIBLE_MS);
    } catch (thrown) {
      const error = toForgeApiError(thrown, { method: "sites.php" });
      if (isNotInstalled(error)) {
        this.pendingTarget = target;
        void this.stopListening();
        this.set({ phase: "confirm_install", target, plan: { status: "loading" } });
        void this.planInstall(target);
        return; // The guard stays: the user decides next.
      }
      this.finish({ phase: "failed", target, error });
    }
  }

  private async planInstall(target: string): Promise<void> {
    const update = (plan: PlanState) => {
      this.update((s) =>
        s.phase === "confirm_install" && s.target === target ? { ...s, plan } : s,
      );
    };
    try {
      const handle = await this.api.job("tools.plan", { specs: [phpSpec(target)] });
      update({ status: "ready", plan: await handle.done });
    } catch (thrown) {
      update({ status: "failed", error: toForgeApiError(thrown, { method: "tools.plan" }) });
    }
  }

  request = (target: string, resolvedBefore: string | null = null): void => {
    if (this.inFlight) return;
    this.inFlight = true;
    this.resolvedBefore = resolvedBefore;
    if (this.doneTimer !== null) clearTimeout(this.doneTimer);
    this.doneTimer = null;
    void this.switchTo(target);
  };

  confirmInstall = (): void => {
    const target = this.pendingTarget;
    if (target === null) return;
    this.pendingTarget = null;
    this.set({ phase: "installing", target, progress: null });
    void (async () => {
      try {
        await runAction(this.runtime.queue, installToolSpec, phpSpec(target), {
          intent: newIntent(),
          onProgress: (progress) => {
            this.update((s) => (s.phase === "installing" ? { ...s, progress } : s));
          },
        });
      } catch (thrown) {
        this.finish({
          phase: "failed",
          target,
          error: toForgeApiError(thrown, { method: "tools.install" }),
        });
        return;
      }
      await this.switchTo(target); // Installed: the same switch again.
    })();
  };

  cancelInstall = (): void => {
    this.pendingTarget = null;
    this.inFlight = false;
    this.update((s) => (s.phase === "confirm_install" ? IDLE : s));
  };

  dismiss = (): void => {
    this.update((s) => (s.phase === "failed" ? IDLE : s));
  };
}

const controllers = new WeakMap<ContextRuntime, Map<string, SwitchController>>();

function controllerFor(runtime: ContextRuntime, siteId: string): SwitchController {
  let bySite = controllers.get(runtime);
  if (bySite === undefined) {
    bySite = new Map();
    controllers.set(runtime, bySite);
  }
  let controller = bySite.get(siteId);
  if (controller === undefined) {
    controller = new SwitchController(runtime.api, runtime, siteId);
    bySite.set(siteId, controller);
  }
  return controller;
}

export function usePhpSwitch(siteId: string): PhpSwitch {
  const runtime = useRuntime();
  const controller = useMemo(() => controllerFor(runtime, siteId), [runtime, siteId]);
  const state = useSyncExternalStore(controller.subscribe, controller.getSnapshot);
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
    request: controller.request,
    confirmInstall: controller.confirmInstall,
    cancelInstall: controller.cancelInstall,
    dismiss: controller.dismiss,
  };
}
