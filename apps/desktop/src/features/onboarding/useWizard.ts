// State of the wizard: the first-run facts, the stored progress and the choices. Every change
// is written through `onboarding_set` at once, so closing the window at any step and opening
// it again resumes where the user stopped (docs/07 section 5.1).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useForgeApi, useHello } from "../../state";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { AutostartStatus } from "../updater/api";
import {
  DEFAULT_ROOT,
  defaultChoices,
  defaultParkedDir,
  isStepId,
  parseChoices,
  visibleSteps,
  type Choices,
  type StepId,
} from "./model";
import { onboardingNative, type FirstRunState } from "./native";

export interface Wizard {
  /** First-run facts and the stored progress are loaded. */
  ready: boolean;
  loadError: ForgeApiError | null;
  firstRun: FirstRunState | null;
  /** The daemon of this context is on this machine. */
  local: boolean;
  /** `portable.flag` decides the root: PATH and the startup entry are not touched. */
  portable: boolean;
  /** Step 1 applies (local, not finished, the root is not forced). */
  showRoot: boolean;
  /** Whether the PATH toggle exists. */
  pathAvailable: boolean;
  autostart: AutostartStatus | null;
  /** Why step 1 / the PATH toggle are hidden, when they are. */
  hiddenReason: "remote" | "portable" | "forced" | null;
  steps: readonly StepId[];
  step: StepId;
  choices: Choices;
  update: (patch: Partial<Choices>) => void;
  /**
   * Writes the whole current step-1 state (and the step) in one `onboarding_set`. Needed right
   * after `choose_root`: the native store keys the progress per root, so what was typed while
   * the old root was current has to be copied to the new key.
   */
  persistAll: (step?: StepId) => Promise<void>;
  go: (step: StepId) => void;
  /** Marks the onboarding done and clears the stored step. */
  finish: () => Promise<void>;
}

export function useWizard(): Wizard {
  const api = useForgeApi();
  const hello = useHello();
  const context = api.context;
  const [firstRun, setFirstRun] = useState<FirstRunState | null>(null);
  const [autostart, setAutostart] = useState<AutostartStatus | null>(null);
  const [loadError, setLoadError] = useState<ForgeApiError | null>(null);
  const [choices, setChoices] = useState<Choices>(defaultChoices());
  const [step, setStep] = useState<StepId>("root");
  const [ready, setReady] = useState(false);
  // The source of truth between renders: `update` writes it before it asks React to render.
  const choicesRef = useRef(choices);
  const stepRef = useRef<StepId>("root");

  useEffect(() => {
    let alive = true;
    const native = onboardingNative();
    Promise.all([native.firstRunState(context), native.get(context), native.autostart()]).then(
      ([state, entry, auto]) => {
        if (!alive) return;
        const root = state.root ?? DEFAULT_ROOT;
        const loaded = parseChoices(entry.choices, root);
        setFirstRun(state);
        setAutostart(auto);
        choicesRef.current = loaded;
        setChoices(loaded);
        const showRoot = state.local && state.show_root_step;
        const first = visibleSteps(showRoot)[0] ?? "stack";
        const initial =
          isStepId(entry.step) && visibleSteps(showRoot).includes(entry.step) && !entry.done
            ? entry.step
            : first;
        stepRef.current = initial;
        setStep(initial);
        setReady(true);
      },
      (error: unknown) => {
        if (!alive) return;
        setLoadError(toForgeApiError(error));
      },
    );
    return () => {
      alive = false;
    };
  }, [context]);

  // A remote daemon (hello) wins over the stored facts: its machine is not this one.
  const local = (firstRun?.local ?? true) && hello?.local !== false;
  const portable = firstRun?.source === "portable";
  const showRoot = local && (firstRun?.show_root_step ?? false);
  const autostartAvailable = local && !portable && autostart?.available === true;
  const hiddenReason: Wizard["hiddenReason"] = !local
    ? "remote"
    : portable
      ? "portable"
      : firstRun !== null && !showRoot && firstRun.source === "env"
        ? "forced"
        : null;

  const update = useCallback(
    (patch: Partial<Choices>) => {
      const next = { ...choicesRef.current, ...patch };
      if (patch.root !== undefined && !next.parkedEdited) {
        next.parkedDir = defaultParkedDir(patch.root);
      }
      choicesRef.current = next;
      setChoices(next);
      const stored: Record<string, unknown> = { ...patch };
      if (patch.root !== undefined) stored.parkedDir = next.parkedDir;
      void onboardingNative()
        .set(context, { choices: stored })
        .catch(() => undefined);
    },
    [context],
  );

  const persistAll = useCallback(
    async (target?: StepId) => {
      const current = choicesRef.current;
      await onboardingNative().set(context, {
        choices: {
          root: current.root,
          parkedDir: current.parkedDir,
          parkedEdited: current.parkedEdited,
          tld: current.tld,
          applied: current.applied,
        },
        step: target ?? stepRef.current,
      });
    },
    [context],
  );

  const go = useCallback(
    (target: StepId) => {
      stepRef.current = target;
      setStep(target);
      void onboardingNative()
        .set(context, { step: target })
        .catch(() => undefined);
    },
    [context],
  );

  const finish = useCallback(async () => {
    await onboardingNative().set(context, { step: "", done: true });
  }, [context]);

  const steps = useMemo(() => visibleSteps(showRoot), [showRoot]);
  return {
    ready,
    loadError,
    firstRun,
    local,
    portable,
    showRoot,
    pathAvailable: local && !portable,
    autostart: autostartAvailable ? autostart : null,
    hiddenReason,
    steps,
    // A step that stopped applying (the daemon turned out to be remote) falls back to the first.
    step: steps.includes(step) ? step : (steps[0] ?? "stack"),
    choices,
    update,
    persistAll,
    go,
    finish,
  };
}
