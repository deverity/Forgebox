import { useState } from "react";
import { useTranslation } from "react-i18next";
import { convertCaseDir } from "../../../../actions/filesystem";
import { Button } from "../../../../design/primitives/Button";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { Select } from "../../../../design/primitives/Select";
import { useFormatters } from "../../../../design/primitives/useFormatters";
import type { CaseConvertDto } from "../../../../ipc/generated";
import { useJob } from "../../../../state";
import { RowFrame } from "../../RowFrame";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";
import { CASE_DIRS, isRevertConflict, joinPath } from "./model";

const TID_FS_CONVERT_RUN = "fs-convert-run";
const TID_FS_CONVERT_REVERT = "fs-convert-revert";
const TID_FS_CONVERT_CANCEL = "fs-convert-cancel";
const TID_FS_CONVERT_CONFLICT = "fs-convert-conflict";
const TID_FS_CONVERT_REPORT = "fs-convert-report";
const TID_FS_REVERT_DIALOG = "fs-revert-dialog";

const METHOD_CONVERT = "fs_parity.case_convert";
const TID_STATUS = "fs-convert-status";

/**
 * Converter job (docs/15 section 3.1): moves a folder into a flagged one (or back, `--revert`)
 * with progress and Cancel. A revert that stops on names that differ only by case is explained
 * instead of shown as a bare error. The job lives in the daemon, so it stays in the global jobs
 * indicator when the user leaves Settings.
 */
export function Converter({ root }: { root: string }) {
  const { t } = useTranslation("settings");
  const format = useFormatters();
  const [name, setName] = useState<string>(CASE_DIRS[0]);
  const [revertAsk, setRevertAsk] = useState(false);
  const [report, setReport] = useState<CaseConvertDto | null>(null);
  const [revert, setRevert] = useState(false);
  const job = useJobAction(convertCaseDir, (result) => {
    setReport(result as CaseConvertDto);
  });
  const view = useJob(job.jobId);
  const dir = root === "" ? "" : joinPath(root, name);
  const noRoot = root === "" ? t("filesystem.table.noRoot") : undefined;
  const start = (asRevert: boolean) => {
    setReport(null);
    setRevert(asRevert);
    job.run({ dir, revert: asRevert });
  };
  const running = job.phase === "running";
  const cancelled = job.phase === "failed" && job.failure?.code === "cancelled";
  const conflict = job.phase === "failed" && isRevertConflict(job.failure, revert);

  return (
    <RowFrame
      id="fs-convert"
      label={t("filesystem.convert.label")}
      description={t("filesystem.convert.description")}
    >
      <div className="flex w-full min-w-96 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Select
            label={t("filesystem.convert.dir")}
            value={name}
            disabled={running}
            options={CASE_DIRS.map((value) => ({ value, label: `${value}/` }))}
            onValueChange={setName}
          />
          <JobButton
            action={convertCaseDir}
            input={{ dir, revert: false }}
            job={job}
            variant="primary"
            disabledReason={noRoot}
            onPress={() => {
              start(false);
            }}
            testId={TID_FS_CONVERT_RUN}
          >
            {t("filesystem.convert.run")}
          </JobButton>
          <JobButton
            action={convertCaseDir}
            input={{ dir, revert: true }}
            job={job}
            disabledReason={noRoot}
            onPress={() => {
              setRevertAsk(true);
            }}
            testId={TID_FS_CONVERT_REVERT}
          >
            {t("filesystem.convert.revert")}
          </JobButton>
          {running ? (
            <Button
              variant="ghost"
              disabled={view.job?.cancel_requested === true}
              data-testid={TID_FS_CONVERT_CANCEL}
              onClick={() => {
                void view.cancel();
              }}
            >
              {view.job?.cancel_requested === true
                ? t("filesystem.convert.cancelling")
                : t("filesystem.convert.cancel")}
            </Button>
          ) : null}
        </div>
        {cancelled ? (
          <div data-testid={TID_STATUS} data-phase="cancelled" className="flex flex-col gap-2">
            <p role="status" className="text-callout">
              {t("filesystem.convert.cancelled")}
            </p>
            <div>
              <Button onClick={job.retry}>{t("filesystem.convert.again")}</Button>
            </div>
          </div>
        ) : conflict && job.failure !== null ? (
          <div data-testid={TID_STATUS} data-phase="conflict" className="flex flex-col gap-2">
            <p data-testid={TID_FS_CONVERT_CONFLICT} role="alert" className="text-callout">
              {t("filesystem.convert.revertConflict")}
            </p>
            <ErrorNotice
              error={job.failure}
              context={{ method: METHOD_CONVERT }}
              onRetry={job.retry}
            />
          </div>
        ) : (
          <JobStatus
            job={job}
            label={t(revert ? "filesystem.convert.reverting" : "filesystem.convert.converting", {
              dir: name,
            })}
            method={METHOD_CONVERT}
            testId={TID_STATUS}
          />
        )}
        {report === null || job.phase !== "done" ? null : (
          <p data-testid={TID_FS_CONVERT_REPORT} className="text-callout text-fg-secondary">
            {report.already
              ? t("filesystem.convert.already")
              : t("filesystem.convert.report", {
                  files: report.files,
                  directories: report.directories,
                  size: format.bytes(report.bytes),
                })}
          </p>
        )}
        <p className="text-caption text-fg-tertiary">{t("filesystem.convert.hint")}</p>
      </div>
      <ConfirmDialog
        open={revertAsk}
        onOpenChange={setRevertAsk}
        title={t("filesystem.convert.revertTitle", { dir: name })}
        description={t("filesystem.convert.revertBody")}
        confirmLabel={t("filesystem.convert.revert")}
        command={convertCaseDir.cli({ dir, revert: true })}
        destructive
        testId={TID_FS_REVERT_DIALOG}
        onConfirm={() => {
          setRevertAsk(false);
          start(true);
        }}
      />
    </RowFrame>
  );
}
