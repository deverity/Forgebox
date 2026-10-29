import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  describeError,
  translateError,
  type DescribableError,
  type ErrorAction,
  type ErrorContext,
  type ErrorTranslate,
} from "../../ipc/describeError";
import { Button } from "./Button";
import { Tooltip } from "./Tooltip";

export interface ErrorNoticeProps {
  error: DescribableError;
  context?: ErrorContext;
  /** Handler of the `retry` action. */
  onRetry?: () => void;
  /** Handler of every other suggested action (start the daemon, show ports...). */
  onAction?: (action: ErrorAction) => void;
  /**
   * Whether the caller can run `action` (never called for `retry`). An action without a
   * handler is not rendered: no inert buttons. Default: every action when `onAction` is set.
   */
  canHandle?: (action: ErrorAction) => boolean;
  /**
   * Why `action` cannot run now (capability: not implemented, local only, role...). The
   * button is shown disabled with this text; `undefined` means it is available.
   */
  disabledReason?: (action: ErrorAction) => string | undefined;
}

/**
 * A failure in plain words: the localized cause, the raw detail on demand and the suggested
 * actions of `describeError` (docs/07 section 1.2: cause + action, never a bare "error").
 */
/** `t` of the `errors` namespace as `describeError` consumers need it. */
export function useErrorTranslate(): ErrorTranslate {
  const { t } = useTranslation("errors");
  return t as unknown as ErrorTranslate;
}

export function ErrorNotice({
  error,
  context,
  onRetry,
  onAction,
  canHandle,
  disabledReason,
}: ErrorNoticeProps) {
  const { t } = useTranslation(["errors", "common"]);
  const [showDetail, setShowDetail] = useState(false);
  const translate = useErrorTranslate();
  const view = translateError(describeError(error, context), translate);
  const buttons: { id: string; text: string; cli?: string | null }[] = view.actions.filter(
    (action) =>
      action.id === "retry"
        ? onRetry !== undefined
        : onAction !== undefined && (canHandle?.(action) ?? true),
  );
  // A caller that can retry always gets the button, even when describeError had no method to
  // repeat (a plain query failure).
  if (onRetry !== undefined && !buttons.some((action) => action.id === "retry")) {
    buttons.unshift({ id: "retry", text: t("common:ui.asyncRetry") });
  }
  return (
    <div role="alert" data-error-notice="" className="flex flex-col gap-2 text-left">
      <p className="text-body text-fg-primary">{view.cause}</p>
      {view.detail === null ? null : (
        <div>
          <Button
            variant="ghost"
            aria-expanded={showDetail}
            onClick={() => {
              setShowDetail((open) => !open);
            }}
          >
            {t("common:ui.errorDetails")}
          </Button>
          {showDetail ? (
            <pre className="mt-1 max-h-32 overflow-auto font-mono text-mono text-fg-secondary">
              {view.detail}
            </pre>
          ) : null}
        </div>
      )}
      {buttons.length > 0 ? (
        <div className="flex flex-wrap gap-2">
          {buttons.map((action) => {
            const full = view.actions.find((a) => a.id === action.id);
            const reason = full === undefined ? undefined : disabledReason?.(full);
            const cli = full?.cli ?? null;
            return (
              <Tooltip
                key={action.id}
                content={reason ?? action.text}
                {...(cli === null ? {} : { command: cli })}
              >
                <span
                  data-forge-cli={cli ?? undefined}
                  data-disabled-reason={reason}
                  // A disabled button cannot take focus: the wrapper does, so the reason is reachable.
                  // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
                  tabIndex={reason === undefined ? undefined : 0}
                  className="inline-flex"
                >
                  <Button
                    variant={action.id === "retry" ? "secondary" : "ghost"}
                    data-error-action={action.id}
                    disabled={reason !== undefined}
                    onClick={() => {
                      if (action.id === "retry") onRetry?.();
                      else if (full !== undefined) onAction?.(full);
                    }}
                  >
                    {action.text}
                  </Button>
                </span>
              </Tooltip>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
