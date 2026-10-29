import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { DescribableError, ErrorAction, ErrorContext } from "../../ipc/describeError";
import { Button } from "./Button";
import { EmptyState } from "./EmptyState";
import { ErrorNotice } from "./ErrorNotice";
import { Skeleton } from "./Skeleton";

export interface AsyncViewProps {
  /** Fixed height in px, the same in every state: swapping states never moves the layout. */
  height: number;
  loading: boolean;
  /** The failure of the data (`ForgeApiError`...). Wins over empty/ready. */
  error?: DescribableError;
  errorContext?: ErrorContext;
  /** No daemon connection: shown instead of an error. */
  disconnected?: boolean;
  /** Start the daemon (disconnected state); omit to hide the button. */
  onStartDaemon?: () => void;
  onRetry?: () => void;
  onErrorAction?: (action: ErrorAction) => void;
  /** The data loaded and there is nothing to show. */
  empty?: boolean;
  emptyTitle?: string;
  emptyDescription?: string;
  /** The empty state's primary action (a Button/ActionButton). */
  emptyAction?: ReactNode;
  /** The data. */
  children: ReactNode;
}

const ROW = 40;
const GAP = 8;

/**
 * Standard data states with one fixed height (docs/07 sections 1.2 and 11): skeleton, empty
 * with an action, error with cause/actions/retry, disconnected, and the data itself.
 */
export function AsyncView({
  height,
  loading,
  error,
  errorContext,
  disconnected = false,
  onStartDaemon,
  onRetry,
  onErrorAction,
  empty = false,
  emptyTitle,
  emptyDescription,
  emptyAction,
  children,
}: AsyncViewProps) {
  const { t } = useTranslation("common");
  const hasError = error !== undefined && error !== null && error !== "";
  let state: "disconnected" | "loading" | "error" | "empty" | "ready";
  if (disconnected) state = "disconnected";
  else if (loading) state = "loading";
  else if (hasError) state = "error";
  else if (empty) state = "empty";
  else state = "ready";

  let body: ReactNode;
  switch (state) {
    case "loading": {
      const rows = Math.max(1, Math.floor((height + GAP) / (ROW + GAP)));
      body = (
        <div className="flex flex-col gap-2">
          <span className="sr-only">{t("ui.asyncLoading")}</span>
          {Array.from({ length: rows }, (_, index) => (
            <Skeleton key={index} height={ROW} />
          ))}
        </div>
      );
      break;
    }
    case "disconnected":
      body = (
        <EmptyState
          title={t("ui.asyncDisconnectedTitle")}
          description={t("ui.asyncDisconnectedHint")}
          {...(onStartDaemon === undefined
            ? {}
            : { action: <Button onClick={onStartDaemon}>{t("ui.asyncRetry")}</Button> })}
        />
      );
      break;
    case "error":
      body = (
        <div className="flex h-full items-center justify-center p-4">
          <ErrorNotice
            error={error}
            {...(errorContext === undefined ? {} : { context: errorContext })}
            {...(onRetry === undefined ? {} : { onRetry })}
            {...(onErrorAction === undefined ? {} : { onAction: onErrorAction })}
          />
        </div>
      );
      break;
    case "empty":
      body = (
        <EmptyState
          title={emptyTitle ?? ""}
          {...(emptyDescription === undefined ? {} : { description: emptyDescription })}
          {...(emptyAction === undefined ? {} : { action: emptyAction })}
        />
      );
      break;
    case "ready":
      body = children;
      break;
  }

  return (
    <div
      data-async-view={state}
      aria-busy={state === "loading"}
      style={{ height }}
      className="overflow-auto"
    >
      {body}
    </div>
  );
}
