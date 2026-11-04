import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Progress } from "../../design/primitives/Progress";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useConnectionStatus, useCapability } from "../../state";
import { CheckRow } from "./CheckRow";
import { DnsTest } from "./DnsTest";
import { ExportButton } from "./ExportButton";
import { groupChecks, totals } from "./model";
import { useDoctor } from "./useDoctor";

const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const DOCTOR_CLI = "forge doctor";

/**
 * Route `/diagnostics` (docs/07 section 5.12): `daemon.doctor` as a job with live progress, the
 * checks grouped by id prefix, allowlisted fixes, "Encerrar" for port conflicts, "Testar DNS" and
 * the exported report (the same JSON as `forge doctor --export`).
 */
export function DiagnosticsPage() {
  const { t } = useTranslation("diagnostics");
  const doctor = useDoctor();
  const status = useConnectionStatus();
  const capability = useCapability("daemon.doctor");
  const reason = useCapabilityReason(capability);
  const [error, setError] = useState<string | null>(null);
  const { state } = doctor;
  const running = state.phase === "running";
  const away = AWAY.has(status);
  const disabled = running || away || reason !== null;
  const checks = state.phase === "done" ? state.checks : [];
  const count = totals(checks);
  return (
    <div
      data-testid="page-diagnostics"
      data-phase={state.phase}
      className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4"
    >
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <div className="flex flex-wrap items-center gap-2">
          <DnsTest />
          <ExportButton checks={checks} disabled={state.phase !== "done"} onError={setError} />
          <Button
            variant="primary"
            loading={running}
            loadingLabel={t("running")}
            disabled={disabled}
            onClick={() => {
              setError(null);
              doctor.run();
            }}
          >
            {state.phase === "done" || state.phase === "failed" ? t("runAgain") : t("run")}
          </Button>
        </div>
      </header>
      {reason !== null && !running ? (
        <p className="text-callout text-fg-secondary">{reason}</p>
      ) : null}
      {error !== null ? (
        <p role="alert" className="text-callout text-state-failed">
          {error}
        </p>
      ) : null}
      {state.phase === "idle" ? (
        <EmptyState
          title={t("idle.title")}
          description={t("idle.description")}
          action={<CliCommand command={DOCTOR_CLI} />}
        />
      ) : null}
      {state.phase === "running" ? (
        <div data-testid="doctor-progress" className="flex max-w-xl flex-col gap-2" role="status">
          <Progress value={state.pct} label={t("progress.label")} />
          <p className="text-callout text-fg-secondary">
            {state.message === null
              ? t("progress.starting")
              : t("progress.step", { step: state.message })}
          </p>
          <Skeleton height={64} />
        </div>
      ) : null}
      {state.phase === "failed" ? <ErrorNotice error={state.error} onRetry={doctor.run} /> : null}
      {state.phase === "done" ? (
        <>
          <p data-testid="doctor-summary" className="text-callout text-fg-secondary">
            {t("summary", { ok: count.ok, warn: count.warn, fail: count.fail })}
            {count.skipped > 0 ? ` ${t("summarySkipped", { count: count.skipped })}` : ""}
          </p>
          {checks.length === 0 ? <EmptyState title={t("none")} /> : null}
          {groupChecks(checks).map((group) => (
            <section
              key={group.group}
              data-group={group.group}
              data-worst={group.worst}
              aria-labelledby={`diag-${group.group}`}
              className="flex flex-col gap-2"
            >
              <h2 id={`diag-${group.group}`} className="text-headline">
                {t(`groups.${group.group}`, { defaultValue: group.group })}
              </h2>
              <ul className="flex flex-col gap-2">
                {group.checks.map((check) => (
                  <CheckRow
                    key={check.id}
                    check={check}
                    busy={doctor.rerunning.has(group.group)}
                    onRerun={() => {
                      void doctor.rerun(group.group).catch(() => {
                        setError(t("rerunFailed"));
                      });
                    }}
                  />
                ))}
              </ul>
            </section>
          ))}
        </>
      ) : null}
    </div>
  );
}
