import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction, type Action } from "../../../actions/registry";
import { CliCommand } from "../../../design/primitives/CliCommand";
import { ErrorNotice, useErrorTranslate } from "../../../design/primitives/ErrorNotice";
import { Toggle } from "../../../design/primitives/Toggle";
import { newIntent } from "../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../ipc/errors";
import type { MethodName } from "../../../ipc/methods";
import { useCapability, useConnectionStatus, useRuntime } from "../../../state";
import { HOLD_STATUSES } from "../../../state/reconnectQueue";

export interface ActionToggleProps<M extends MethodName, I> {
  "data-testid": string;
  label: string;
  description: string;
  action: Action<M, I>;
  /** Current value according to the daemon (the site DTO). */
  checked: boolean;
  /** The action input that sets the toggle to `next`. */
  inputFor: (next: boolean) => I;
  /** Why the toggle cannot be used at all (e.g. the site is not PHP); wins over capabilities. */
  disabledReason?: string;
}

type Phase = "idle" | "running" | "done" | "failed";

/**
 * A switch bound to a registered action (docs/07 sections 1.2, 11): while the call (or job) runs
 * the switch shows the requested value, disabled, with "Applying..."; success keeps it and says
 * so; failure puts it back and shows the cause with a retry. The equivalent `forge` command of
 * the change is always visible (also in `data-forge-cli`), so a toggle never hides its CLI.
 */
export function ActionToggle<M extends MethodName, I>({
  "data-testid": testId,
  label,
  description,
  action,
  checked,
  inputFor,
  disabledReason,
}: ActionToggleProps<M, I>) {
  const { t } = useTranslation("metrics");
  const translate = useErrorTranslate();
  const runtime = useRuntime();
  const capability = useCapability(action.method);
  const status = useConnectionStatus();
  const [phase, setPhase] = useState<Phase>("idle");
  const [target, setTarget] = useState<boolean | null>(null);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  // The value the daemon confirmed, shown until the site store catches up.
  const [confirmed, setConfirmed] = useState<{ value: boolean; from: boolean } | null>(null);
  const alive = useRef(true);
  const descId = useId();

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const held = capability.reason === "disconnected" && HOLD_STATUSES.has(status);
  const capabilityReason =
    capability.text === null || held
      ? undefined
      : translate(capability.text.key, capability.text.values);
  const reason = disabledReason ?? capabilityReason;
  const running = phase === "running";
  // The confirmed value is shown only until the store moves off the value it was set from
  // (our own change arriving, or someone else's): after that the store is the truth again.
  const stale = confirmed !== null && confirmed.from === checked ? confirmed.value : checked;
  const shown = running && target !== null ? target : stale;

  const run = (next: boolean) => {
    if (running) return;
    setTarget(next);
    setPhase("running");
    setFailure(null);
    runAction(runtime.queue, action, inputFor(next), { intent: newIntent() }).then(
      () => {
        if (!alive.current) return;
        setConfirmed({ value: next, from: checked });
        setPhase("done");
      },
      (error: unknown) => {
        if (!alive.current) return;
        setConfirmed(null);
        setFailure(toForgeApiError(error, { method: action.method }));
        setPhase("failed");
      },
    );
  };

  // The command of the change the switch would make now (or is making).
  const next =
    running && target !== null ? target : phase === "failed" && target !== null ? target : !shown;
  let cli: string | null = null;
  try {
    cli = action.cli(inputFor(next));
  } catch {
    cli = null;
  }

  return (
    <div
      data-testid={testId}
      data-phase={phase}
      data-checked={shown}
      {...(cli === null ? {} : { "data-forge-cli": cli })}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-start gap-3">
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="text-headline text-fg-primary">{label}</span>
          <p id={descId} className="text-callout text-fg-secondary">
            {description}
          </p>
          {reason !== undefined ? (
            <p data-testid={`${testId}-reason`} className="text-callout text-fg-tertiary">
              {reason}
            </p>
          ) : null}
        </div>
        <div className="flex items-center gap-2 pt-0.5">
          <span
            role="status"
            data-testid={`${testId}-status`}
            className="min-w-24 text-right text-caption text-fg-secondary"
          >
            {running
              ? t("site.controls.applying")
              : phase === "done"
                ? t("site.controls.done")
                : ""}
          </span>
          <Toggle
            label={label}
            checked={shown}
            disabled={reason !== undefined || running}
            onCheckedChange={run}
          />
        </div>
      </div>
      <CliCommand command={cli} />
      {phase === "failed" && failure !== null ? (
        <ErrorNotice
          error={failure}
          context={{ method: action.method, params: action.toParams(inputFor(next)) }}
          onRetry={() => {
            run(next);
          }}
        />
      ) : null}
    </div>
  );
}
