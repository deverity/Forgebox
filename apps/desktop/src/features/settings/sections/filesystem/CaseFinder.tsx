import { useState } from "react";
import { useTranslation } from "react-i18next";
import { lintSiteCase } from "../../../../actions/filesystem";
import { useCapabilityReason } from "../../../../app/shell/useErrorText";
import { Badge } from "../../../../design/primitives/Badge";
import { Button } from "../../../../design/primitives/Button";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { Select } from "../../../../design/primitives/Select";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { CaseFixChoice, CaseIssueDto, CaseLintReportDto } from "../../../../ipc/generated";
import { useCapability, useForgeApi, useForgeQuery } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";
import { CASE_STRATEGIES, fixableCount, issuePath } from "./model";

const TID_FINDER_SCAN = "finder-scan";
const TID_FINDER_SCAN_STATUS = "finder-scan-status";
const TID_FINDER_EMPTY = "finder-empty";
const TID_FINDER_REPORT = "finder-report";
const TID_FINDER_APPLY = "finder-apply";
const TID_FINDER_APPLY_STATUS = "finder-apply-status";
const TID_FINDER_DIALOG = "finder-dialog";
const ARROW = " → ";
const METHOD_OPEN_EDITOR = "shell.open_editor";

const METHOD_LINT = "fs_parity.lint_case";

function Finding({
  issue,
  root,
  canOpen,
  onOpen,
}: {
  issue: CaseIssueDto;
  root: string;
  canOpen: boolean;
  onOpen: (path: string, line: number, column: number) => void;
}) {
  const { t } = useTranslation("settings");
  const where = issue.line > 0 ? `${issue.file}:${String(issue.line)}` : issue.file;
  const fix = issue.fix;
  return (
    <li
      data-testid={`finding-${issue.id}`}
      data-category={issue.category}
      className="flex flex-col gap-1 py-2"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="ghost"
          disabled={!canOpen}
          data-testid={`finding-open-${issue.id}`}
          onClick={() => {
            onOpen(issuePath(root, issue.file), issue.line, issue.column);
          }}
        >
          <span className="font-mono text-mono">{where}</span>
        </Button>
        <Badge>{loose(t)(`filesystem.finder.categories.${issue.category}`)}</Badge>
      </div>
      <p className="text-callout text-fg-secondary">
        <code>{issue.reference}</code>
        {issue.actual === "" || issue.actual === issue.reference ? null : (
          <>
            {ARROW}
            <code>{issue.actual}</code>
          </>
        )}
        {issue.detail === null ? null : ` · ${issue.detail}`}
      </p>
      {fix === null ? null : (
        <p data-testid={`finding-fix-${issue.id}`} className="text-callout">
          {loose(t)(`filesystem.finder.strategies.${fix.strategy}`)}:{" "}
          <code>{fix.before ?? fix.from}</code>
          {ARROW}
          <code>{fix.after ?? fix.to}</code>
        </p>
      )}
    </li>
  );
}

/**
 * Case Mismatch Finder (docs/15 section 6): `lint_case` as a dry run lists each finding as
 * `file:line` (opens the editor there), with the fix it would make; applying is a second job
 * that only starts after the confirmation.
 */
export function CaseFinder() {
  const { t } = useTranslation("settings");
  const api = useForgeApi();
  const sites = useForgeQuery("sites.list", {});
  const editor = useCapability("shell.open_editor");
  const editorReason = useCapabilityReason(editor);
  const [site, setSite] = useState<string | null>(null);
  const [strategy, setStrategy] = useState<CaseFixChoice>("auto");
  const [report, setReport] = useState<CaseLintReportDto | null>(null);
  const [applied, setApplied] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [editorFailure, setEditorFailure] = useState<ForgeApiError | null>(null);
  const list = sites.data?.sites ?? [];
  const chosen = site ?? list[0]?.host ?? null;
  const scan = useJobAction(lintSiteCase, (result) => {
    setReport(result as CaseLintReportDto);
    setApplied(false);
  });
  const apply = useJobAction(lintSiteCase, (result) => {
    setReport(result as CaseLintReportDto);
    setApplied(true);
  });

  const openEditor = (path: string, line: number, column: number) => {
    setEditorFailure(null);
    api
      .call("shell.open_editor", {
        path,
        ...(line > 0 ? { line, column: column > 0 ? column : 1 } : {}),
      })
      .catch((error: unknown) => {
        setEditorFailure(toForgeApiError(error));
      });
  };

  const scanInput = { site: chosen ?? "", strategy, dryRun: true };
  const applyInput = { site: chosen ?? "", strategy, dryRun: false };
  const fixable = report === null ? 0 : fixableCount(report.issues);
  const noSite = chosen === null ? t("filesystem.finder.noSites") : undefined;

  return (
    <RowFrame
      id="fs-finder"
      label={t("filesystem.finder.label")}
      description={t("filesystem.finder.description")}
    >
      <div className="flex w-full min-w-96 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Select
            label={t("filesystem.finder.site")}
            value={chosen ?? ""}
            disabled={list.length === 0}
            options={list.map((entry) => ({ value: entry.host, label: entry.host }))}
            placeholder={t("filesystem.finder.noSites")}
            onValueChange={(value) => {
              setSite(value);
              setReport(null);
              scan.reset();
              apply.reset();
            }}
          />
          <Select
            label={t("filesystem.finder.strategy")}
            value={strategy}
            width="sm"
            options={CASE_STRATEGIES.map((value) => ({
              value,
              label: loose(t)(`filesystem.finder.strategies.${value}`),
            }))}
            onValueChange={(value) => {
              setStrategy(value as CaseFixChoice);
            }}
          />
          <JobButton
            action={lintSiteCase}
            input={scanInput}
            job={scan}
            variant="primary"
            disabledReason={noSite}
            onPress={() => {
              setReport(null);
              apply.reset();
              scan.run(scanInput);
            }}
            testId={TID_FINDER_SCAN}
          >
            {t("filesystem.finder.scan")}
          </JobButton>
        </div>
        <JobStatus
          job={scan}
          label={t("filesystem.finder.scanning")}
          method={METHOD_LINT}
          testId={TID_FINDER_SCAN_STATUS}
        />
        {editorFailure === null ? null : (
          <ErrorNotice error={editorFailure} context={{ method: METHOD_OPEN_EDITOR }} />
        )}
        {report === null ? null : report.issues.length === 0 ? (
          <p data-testid={TID_FINDER_EMPTY} role="status" className="text-callout">
            {applied
              ? t("filesystem.finder.appliedClean")
              : t("filesystem.finder.clean", { count: report.files_scanned })}
          </p>
        ) : (
          <div
            data-testid={TID_FINDER_REPORT}
            data-applied={applied}
            className="flex flex-col gap-2"
          >
            <p className="text-callout" role="status">
              {applied
                ? t("filesystem.finder.appliedSummary", { remaining: report.remaining ?? 0 })
                : t("filesystem.finder.summary", {
                    count: report.issues.length,
                    fixable,
                  })}
            </p>
            {editor.available ? null : (
              <p className="text-caption text-fg-tertiary">{editorReason}</p>
            )}
            <ul className="divide-y divide-stroke">
              {report.issues.map((issue) => (
                <Finding
                  key={issue.id}
                  issue={issue}
                  root={report.root}
                  canOpen={editor.available}
                  onOpen={openEditor}
                />
              ))}
            </ul>
            {applied || fixable === 0 ? null : (
              <div className="flex flex-col gap-2">
                <p className="text-caption text-fg-secondary">
                  {t("filesystem.finder.dryRunNote")}
                </p>
                <div>
                  <JobButton
                    action={lintSiteCase}
                    input={applyInput}
                    job={apply}
                    variant="destructive"
                    onPress={() => {
                      setConfirm(true);
                    }}
                    testId={TID_FINDER_APPLY}
                  >
                    {t("filesystem.finder.apply", { count: fixable })}
                  </JobButton>
                </div>
              </div>
            )}
          </div>
        )}
        <JobStatus
          job={apply}
          label={t("filesystem.finder.applying")}
          method={METHOD_LINT}
          testId={TID_FINDER_APPLY_STATUS}
        />
        <p className="text-caption text-fg-tertiary">{t("filesystem.finder.hint")}</p>
      </div>
      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        title={t("filesystem.finder.confirmTitle", { site: chosen ?? "" })}
        description={t("filesystem.finder.confirmBody", { count: fixable })}
        confirmLabel={t("filesystem.finder.confirmApply")}
        command={lintSiteCase.cli(applyInput)}
        destructive
        testId={TID_FINDER_DIALOG}
        onConfirm={() => {
          setConfirm(false);
          apply.run(applyInput);
        }}
      />
    </RowFrame>
  );
}
