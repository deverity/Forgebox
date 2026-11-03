import { useTranslation } from "react-i18next";
import { Button } from "../../../design/primitives/Button";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Progress } from "../../../design/primitives/Progress";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { useJob } from "../../../state";
import { barValue, positive } from "./model";

/** One install the list follows. */
export interface InstallProgressItem {
  /** Stable key of the row. */
  key: string;
  /** The `job_id` returned by `tools.install`. */
  jobId: string;
  /** What is installing ("PHP 8.4.27 (nts)"). */
  label: string;
}

export interface InstallProgressListProps {
  items: readonly InstallProgressItem[];
  /** Retry a failed or cancelled row (the caller starts the install again). */
  onRetry?: (item: InstallProgressItem) => void;
  /** Remove a finished row from the list. */
  onDismiss?: (item: InstallProgressItem) => void;
}

interface RowProps {
  item: InstallProgressItem;
  onRetry: ((item: InstallProgressItem) => void) | undefined;
  onDismiss: ((item: InstallProgressItem) => void) | undefined;
}

function InstallProgressRow({ item, onRetry, onDismiss }: RowProps) {
  const { t } = useTranslation("catalog");
  const job = useJob(item.jobId);
  const format = useFormatters();
  const { state, progress, error } = job;
  const done = state === "done";
  const cancelled = state === "cancelled";
  const failed = state === "failed" && error !== null;
  const message = progress.message ?? null;
  const speed = positive(progress.speed_bps);
  const eta = positive(progress.eta_secs);
  const value = barValue(progress);
  const bytesDone = progress.bytes_done;
  const bytesTotal = progress.bytes_total;
  let bytesText: string | null = null;
  if (typeof bytesDone === "number") {
    bytesText =
      typeof bytesTotal === "number" && bytesTotal > 0
        ? t("installProgress.bytesOf", {
            done: format.bytes(bytesDone),
            total: format.bytes(bytesTotal),
          })
        : format.bytes(bytesDone);
  }
  const stepText = [bytesText, message ?? (value === null ? t("installProgress.working") : null)]
    .filter((part): part is string => part !== null && part !== "")
    .join(" · ");
  return (
    <li
      data-install-job={item.jobId}
      data-state={cancelled ? "cancelled" : state}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-body">{item.label}</span>
        {job.running || state === "unknown" ? (
          <Button
            variant="ghost"
            disabled={job.job?.cancel_requested === true}
            onClick={() => {
              void job.cancel();
            }}
          >
            {job.job?.cancel_requested === true
              ? t("installProgress.cancelling")
              : t("installProgress.cancel")}
          </Button>
        ) : null}
        {done || cancelled || failed ? (
          <>
            {failed || cancelled ? (
              onRetry === undefined ? null : (
                <Button
                  variant="ghost"
                  onClick={() => {
                    onRetry(item);
                  }}
                >
                  {t("installProgress.retry")}
                </Button>
              )
            ) : null}
            {onDismiss === undefined ? null : (
              <Button
                variant="ghost"
                onClick={() => {
                  onDismiss(item);
                }}
              >
                {t("installProgress.dismiss")}
              </Button>
            )}
          </>
        ) : null}
      </div>
      {cancelled ? (
        <p className="text-callout text-fg-secondary" role="status">
          {t("installProgress.cancelled")}
        </p>
      ) : failed ? (
        <ErrorNotice
          error={error}
          {...(onRetry === undefined
            ? {}
            : {
                onRetry: () => {
                  onRetry(item);
                },
              })}
        />
      ) : (
        <>
          <Progress
            label={t("installProgress.label", { name: item.label })}
            value={done ? 100 : value}
            done={done}
            {...(speed === undefined || done ? {} : { speedBps: speed })}
            {...(eta === undefined || done ? {} : { etaSeconds: eta })}
          />
          <p className="text-caption text-fg-secondary" data-part="step" role="status">
            {done ? t("installProgress.done") : stepText}
          </p>
        </>
      )}
    </li>
  );
}

/**
 * The installs in flight (catalog and onboarding): determinate bar with bytes, speed and ETA
 * when the daemon knows the total, indeterminate bar with the step text when it does not,
 * Cancel (`jobs.cancel`) and a morph to a check when the job is done. Follows each job
 * through the jobs store (events plus the safety poll), so it survives re-renders.
 */
export function InstallProgressList({ items, onRetry, onDismiss }: InstallProgressListProps) {
  const { t } = useTranslation("catalog");
  if (items.length === 0) return null;
  return (
    <ul aria-label={t("installProgress.list")} className="flex flex-col gap-2">
      {items.map((item) => (
        <InstallProgressRow key={item.key} item={item} onRetry={onRetry} onDismiss={onDismiss} />
      ))}
    </ul>
  );
}
