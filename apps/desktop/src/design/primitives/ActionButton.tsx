import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { runAction, type Action } from "../../actions/registry";
import type { ErrorAction } from "../../ipc/describeError";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { MethodName } from "../../ipc/methods";
import { newIntent } from "../../ipc/api";
import { useCapabilities, useCapability, useConnectionStatus, useRuntime } from "../../state";
import { HOLD_STATUSES } from "../../state/reconnectQueue";
import { Button, type ButtonVariant } from "./Button";
import { ErrorNotice, useErrorTranslate } from "./ErrorNotice";
import { Tooltip } from "./Tooltip";

const CHECK = "✓";
const DONE_MS = 1800;

export interface ActionButtonProps<M extends MethodName, I> {
  action: Action<M, I>;
  /** What the action's `toParams` takes (omit for `void`). */
  input?: I;
  variant?: ButtonVariant;
  /** Idle label. */
  children: ReactNode;
  /** Localized text while the call runs ("Starting"); defaults to the idle label. */
  loadingLabel?: string;
  /** Localized text of the success check (default: "Done"). */
  successLabel?: string;
  /** Extra reason that disables the button (validation); wins over nothing else. */
  disabledReason?: string;
  /** The action finished; `result` is the call result or the job result. */
  onDone?: (result: unknown) => void;
  /** An error action other than retry / start-daemon (show ports, open logs...). */
  onErrorAction?: (action: ErrorAction) => void;
}

/**
 * Button bound to a registered action (docs/07 sections 1.2, 1.7, 11): loading text in the
 * same frame as the click, a success check, and on failure the localized cause plus the
 * actions of `describeError`. Disabled with a visible reason when the capabilities say
 * not_implemented, local_only on a remote context, insufficient role, or the daemon is
 * deliberately away (uninitialized, stopped, closed). While the daemon is coming back (idle,
 * connecting, reconnecting, starting) the button stays enabled: the click goes through the
 * ReconnectQueue, which holds the call (same `request_id`) for up to 30 s (ADR-0034 section 11,
 * docs/03 section 7.5) and the button shows "Waiting for the daemon" meanwhile. Its
 * tooltip always shows the equivalent `forge` command, also in `data-forge-cli`.
 */
export function ActionButton<M extends MethodName, I = void>({
  action,
  input,
  variant = "secondary",
  children,
  loadingLabel,
  successLabel,
  disabledReason,
  onDone,
  onErrorAction,
}: ActionButtonProps<M, I>) {
  const { t } = useTranslation("common");
  const translate = useErrorTranslate();
  const runtime = useRuntime();
  const capability = useCapability(action.method);
  const status = useConnectionStatus();
  const capabilities = useCapabilities();
  const [phase, setPhase] = useState<"idle" | "running" | "done" | "failed">("idle");
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const doneTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const alive = useRef(true);
  const reasonId = useId();

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (doneTimer.current !== null) clearTimeout(doneTimer.current);
    };
  }, []);

  let cli: string | null = null;
  try {
    cli = action.cli(input as I);
  } catch {
    cli = null; // The input is not complete yet: no command to show.
  }
  // `disconnected` only means "no hello yet": when the queue holds calls the click is allowed.
  const held = capability.reason === "disconnected" && HOLD_STATUSES.has(status);
  const reason =
    disabledReason ??
    (capability.text === null || held
      ? undefined
      : translate(capability.text.key, capability.text.values));
  const disabled = reason !== undefined;

  const run = () => {
    if (phase === "running") return;
    // Same frame: the loading text is rendered by this very state update.
    setPhase("running");
    setFailure(null);
    runAction(runtime.queue, action, input as I, { intent: newIntent() }).then(
      (result) => {
        if (!alive.current) return;
        setPhase("done");
        doneTimer.current = setTimeout(() => {
          if (alive.current) setPhase("idle");
        }, DONE_MS);
        onDone?.(result);
      },
      (error: unknown) => {
        if (!alive.current) return;
        setFailure(toForgeApiError(error, { method: action.method }));
        setPhase("failed");
      },
    );
  };

  const onAction = (errorAction: ErrorAction) => {
    if (errorAction.client === "start_daemon") void runtime.api.start();
    else onErrorAction?.(errorAction);
  };
  // Only what has a handler is offered: start_daemon is ours, the rest is the caller's.
  const canHandle = (errorAction: ErrorAction) =>
    errorAction.client === "start_daemon" || onErrorAction !== undefined;
  // Every action with a method obeys the capabilities (role, local_only, not_implemented).
  const actionReason = (errorAction: ErrorAction) => {
    if (errorAction.method === null) return undefined;
    const { text } = capabilities.can(errorAction.method);
    return text === null ? undefined : translate(text.key, text.values);
  };

  const label =
    phase === "done" ? (
      <>
        <span aria-hidden="true">{CHECK}</span>
        {successLabel ?? t("ui.actionDone")}
      </>
    ) : (
      children
    );

  return (
    <div className="inline-flex flex-col items-start gap-2">
      <Tooltip content={reason ?? children} {...(cli === null ? {} : { command: cli })}>
        <span
          data-action={action.id}
          data-phase={phase}
          data-forge-cli={cli ?? undefined}
          data-disabled-reason={reason}
          // A disabled button cannot take focus: the wrapper does, so keyboard users reach the reason.
          // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
          tabIndex={disabled ? 0 : undefined}
          className="inline-flex"
        >
          <Button
            variant={variant}
            loading={phase === "running"}
            {...(loadingLabel === undefined && !held
              ? {}
              : { loadingLabel: held ? t("ui.actionWaiting") : loadingLabel })}
            disabled={disabled}
            aria-describedby={disabled ? reasonId : undefined}
            onClick={run}
          >
            {label}
          </Button>
          {disabled ? (
            <span id={reasonId} className="sr-only">
              {reason}
            </span>
          ) : null}
        </span>
      </Tooltip>
      {phase === "failed" && failure !== null ? (
        <ErrorNotice
          error={failure}
          context={{ method: action.method, params: action.toParams(input as I) }}
          onRetry={run}
          onAction={onAction}
          canHandle={canHandle}
          disabledReason={actionReason}
        />
      ) : null}
    </div>
  );
}
