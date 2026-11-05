import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import type { ErrorAction, ErrorContext } from "../../ipc/describeError";
import { useConnectionStatus, useRuntime, useSliceError } from "../../state";
import type { SliceName } from "../../state/stores";

/** Connection states in which nothing is coming: the daemon stopped on purpose or never ran. */
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);

export interface NoDataStateProps {
  slice: SliceName;
  /** The call that fills the slice (for the "run it yourself" command of the error). */
  context: ErrorContext;
  /** What to show while the first load is genuinely in flight. */
  loading: ReactNode;
}

/**
 * What a screen shows while its snapshot slice has no data (docs/07 sections 1.2 and 11):
 * the failure with its cause and a retry, the daemon being away with a start button, or the
 * loading skeleton. Never an endless skeleton: a failed or `not_implemented` list explains
 * itself (M2-E4-T9, R-09, R-10, R-16).
 */
export function NoDataState({ slice, context, loading }: NoDataStateProps) {
  const { t } = useTranslation("common");
  const runtime = useRuntime();
  const status = useConnectionStatus();
  const error = useSliceError(slice);
  if (error !== null) {
    return (
      <div className="flex justify-center p-4" data-testid="no-data-error">
        <ErrorNotice
          error={error}
          context={context}
          onRetry={() => {
            runtime.refresh(slice);
          }}
          onAction={(action: ErrorAction) => {
            if (action.client === "start_daemon") void runtime.api.start();
          }}
        />
      </div>
    );
  }
  if (AWAY.has(status)) {
    return (
      <EmptyState
        title={t("ui.asyncDisconnectedTitle")}
        description={t("ui.asyncDisconnectedHint")}
        action={
          <Button
            onClick={() => {
              void runtime.api.start();
            }}
          >
            {t("ui.asyncRetry")}
          </Button>
        }
      />
    );
  }
  return <>{loading}</>;
}
