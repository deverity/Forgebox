import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Progress } from "../../design/primitives/Progress";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import { revealPath } from "../../ipc/native";
import { useHello, useJob, useRuntime } from "../../state";
import { barValue, positive } from "../shared/install-progress/model";
import { exportResult, stderrTail } from "./model";
import { dismissTransfer, startTransfer, useTransfers, type Transfer } from "./transfers";

const SEPARATOR = " · ";

/** One export or import job: progress, cancel, the outcome and, when it failed, the client output. */
function TransferRow({ transfer }: { transfer: Transfer }) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const format = useFormatters();
  const hello = useHello();
  const job = useJob(transfer.jobId);
  const [retryFailure, setRetryFailure] = useState<ForgeApiError | null>(null);
  const { input } = transfer;
  const isExport = input.kind === "export";
  const { state, progress, error } = job;
  const done = state === "done";
  const cancelled = state === "cancelled";
  const failed = state === "failed" && error !== null;
  const value = barValue(progress);
  const written = progress.bytes_done;
  const speed = positive(progress.speed_bps);
  const exported = done && isExport ? exportResult(job.result) : null;
  const local = hello?.local === true;
  const cancelling = job.job?.cancel_requested === true;

  const retry = () => {
    setRetryFailure(null);
    startTransfer(runtime, input, transfer.jobId).catch((failure: unknown) => {
      setRetryFailure(toForgeApiError(failure, { method: `db.${input.kind}` }));
    });
  };
  const dismiss = () => {
    dismissTransfer(transfer.jobId);
  };

  const detail: string[] = [];
  if (isExport && typeof written === "number") {
    detail.push(t("export.bytes", { size: format.bytes(written) }));
  }
  if (isExport && speed !== undefined) {
    detail.push(t("export.speed", { speed: format.bytes(speed) }));
  }

  return (
    <li
      data-transfer={transfer.jobId}
      data-kind={input.kind}
      data-state={state}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-body">
          {t(isExport ? "export.label" : "import.label", { name: input.name })}
        </span>
        {job.running || state === "unknown" ? (
          <Button
            variant="ghost"
            disabled={cancelling}
            onClick={() => {
              void job.cancel();
            }}
          >
            {t(
              cancelling
                ? isExport
                  ? "export.cancelling"
                  : "import.cancelling"
                : isExport
                  ? "export.cancel"
                  : "import.cancel",
            )}
          </Button>
        ) : null}
        {failed || cancelled ? (
          <Button variant="ghost" onClick={retry}>
            {t(isExport ? "export.retry" : "import.retry")}
          </Button>
        ) : null}
        {done || failed || cancelled ? (
          <Button variant="ghost" onClick={dismiss}>
            {t(isExport ? "export.dismiss" : "import.dismiss")}
          </Button>
        ) : null}
      </div>
      {cancelled ? (
        <p role="status" className="text-callout text-fg-secondary">
          {t(isExport ? "export.cancelled" : "import.cancelled")}
        </p>
      ) : failed ? (
        <div className="flex flex-col gap-2" data-testid="transfer-failure">
          <p className="text-callout text-state-failed">
            {t(isExport ? "export.failedTitle" : "import.failedTitle")}
          </p>
          <ErrorNotice error={error} />
          {stderrTail(error.message) === "" ? null : (
            <pre
              aria-label={t(isExport ? "export.stderr" : "import.stderr")}
              data-testid="transfer-stderr"
              className="max-h-40 overflow-auto rounded-control border border-stroke bg-surface-2 p-2 font-mono text-mono text-fg-secondary"
            >
              {stderrTail(error.message)}
            </pre>
          )}
        </div>
      ) : done ? (
        <div className="flex items-center gap-2">
          <p role="status" className={`min-w-0 flex-1 truncate text-callout ${TNUM_CLASS}`}>
            {exported === null
              ? t("import.done")
              : t("export.done", { path: exported.path, size: format.bytes(exported.size) })}
          </p>
          {exported !== null && local ? (
            <Button
              variant="secondary"
              onClick={() => {
                void revealPath(exported.path);
              }}
            >
              {t("export.reveal")}
            </Button>
          ) : null}
        </div>
      ) : (
        <>
          <Progress
            label={t(isExport ? "export.label" : "import.label", { name: input.name })}
            value={isExport ? null : value}
          />
          {isExport || value === null ? (
            <p
              role="status"
              data-part="step"
              className={`text-caption text-fg-secondary ${TNUM_CLASS}`}
            >
              {isExport
                ? detail.length > 0
                  ? detail.join(SEPARATOR)
                  : t("export.starting")
                : t("import.starting")}
            </p>
          ) : null}
        </>
      )}
      {retryFailure === null ? null : <ErrorNotice error={retryFailure} />}
    </li>
  );
}

/** The transfers of the current context (the screen and the site tab share the list). */
export function TransferList({ instance }: { instance?: string }) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const items = useTransfers().filter(
    (item) =>
      item.context === runtime.context &&
      (instance === undefined || item.input.instance === instance),
  );
  if (items.length === 0) return null;
  return (
    <section aria-label={t("jobs.title")} data-testid="transfers" className="flex flex-col gap-2">
      <h2 className="text-headline">{t("jobs.title")}</h2>
      <ul className="flex flex-col gap-2">
        {items.map((item) => (
          <TransferRow key={item.jobId} transfer={item} />
        ))}
      </ul>
    </section>
  );
}
