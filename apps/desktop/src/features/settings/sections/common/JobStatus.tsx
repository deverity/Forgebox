import { useTranslation } from "react-i18next";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { Progress } from "../../../../design/primitives/Progress";
import { useJob } from "../../../../state";
import type { JobRun } from "./useJobAction";

const CHECK = "✓";

export interface JobStatusProps<I> {
  job: JobRun<I>;
  /** Accessible name of the progress bar ("Enabling the local DNS"). */
  label: string;
  /** The job asks Windows for administrator rights: say so while it waits (UAC). */
  elevated?: boolean;
  /** Method named in the failure ("net.dns_enable"), for the retry. */
  method: string;
  testId: string;
}

/**
 * What a settings card shows around a job so the UI never looks frozen (docs/07 section 5.11):
 * an indeterminate bar, the step the daemon reports, the UAC notice when the job elevates and,
 * on failure, the cause with a retry (`needs_elevation`: the prompt was refused or never shown).
 */
export function JobStatus<I>({ job, label, elevated = false, method, testId }: JobStatusProps<I>) {
  const { t } = useTranslation("settings");
  const view = useJob(job.jobId);
  if (job.phase === "idle") return null;
  if (job.phase === "running") {
    const pct = view.progress.pct ?? null;
    const message = view.progress.message ?? null;
    return (
      <div data-testid={testId} data-phase="running" role="status" className="flex flex-col gap-2">
        <Progress value={pct} label={label} />
        {message === null ? null : <p className="text-callout text-fg-secondary">{message}</p>}
        {elevated ? (
          <p data-testid={`${testId}-uac`} className="text-callout text-fg-secondary">
            {t("job.uac")}
          </p>
        ) : null}
      </div>
    );
  }
  if (job.phase === "failed" && job.failure !== null) {
    return (
      <div data-testid={testId} data-phase="failed" className="flex flex-col gap-1">
        <ErrorNotice error={job.failure} context={{ method }} onRetry={job.retry} />
      </div>
    );
  }
  return (
    <p data-testid={testId} data-phase="done" role="status" className="text-callout">
      <span aria-hidden="true">{CHECK}</span> {t("job.done")}
    </p>
  );
}
