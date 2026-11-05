import { useState } from "react";
import { useTranslation } from "react-i18next";
import { installCa } from "../../../../actions/diagnostics";
import { runAction } from "../../../../actions/registry";
import { exportCa, uninstallCa } from "../../../../actions/security";
import { Badge } from "../../../../design/primitives/Badge";
import { Button } from "../../../../design/primitives/Button";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { SegmentedControl } from "../../../../design/primitives/SegmentedControl";
import { toast } from "../../../../design/primitives/toastStore";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { PemDto, TrustScopeDto } from "../../../../ipc/generated";
import { saveCaCertificate } from "../../../../ipc/native";
import { useCapability, useForgeQuery, useRuntime } from "../../../../state";
import { RowFrame } from "../../RowFrame";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { RotateCa } from "../common/RotateCa";
import { useJobAction } from "../common/useJobAction";

const CAPTION_FORGE_TLS_CA_STATUS = "forge tls ca status";
const METHOD_TLS_CA_EXPORT = "tls.ca_export";
const METHOD_TLS_CA_INSTALL = "tls.ca_install";
const METHOD_TLS_CA_STATUS = "tls.ca_status";
const METHOD_TLS_CA_UNINSTALL = "tls.ca_uninstall";
const SCOPE_CURRENT_USER = "current_user";
const SCOPE_LOCAL_MACHINE = "local_machine";
const TID_CA_EXPORT = "ca-export";
const TID_CA_INSTALL = "ca-install";
const TID_CA_INSTALL_NOTE = "ca-install-note";
const TID_CA_INSTALL_STATUS = "ca-install-status";
const TID_CA_ROTATE = "ca-rotate";
const TID_CA_STATUS = "ca-status";
const TID_CA_UNINSTALL_MACHINE = "ca-uninstall-machine";
const TID_CA_UNINSTALL_MACHINE_STATUS = "ca-uninstall-machine-status";
const TID_CA_UNINSTALL_USER = "ca-uninstall-user";
const TID_CA_UNINSTALL_USER_STATUS = "ca-uninstall-user-status";

/** CLI shown under the export button; the GUI saves through a dialog, the CLI takes `-o`. */
const EXPORT_HINT = "forge tls ca export -o ca.crt";

/**
 * The local CA (docs/11 section 3): status, trust per store, and the five things one does with
 * it. Install in the user store shows a Windows security dialog; in the machine store it
 * needs administrator rights (UAC through `forgebox-helper`). Export reads the public
 * certificate and saves it with the native dialog. Rotate is destructive (modal). Uninstall
 * removes it from one store.
 */
export function CaCard() {
  const { t } = useTranslation("settings");
  const runtime = useRuntime();
  const status = useForgeQuery("tls.ca_status", {});
  const exportCapability = useCapability("tls.ca_export");
  const [scope, setScope] = useState<TrustScopeDto>("current_user");
  const install = useJobAction(installCa);
  const uninstallUser = useJobAction(uninstallCa);
  const uninstallMachine = useJobAction(uninstallCa);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<ForgeApiError | null>(null);
  const ca = status.data;
  const machine = scope === "local_machine";

  const doExport = () => {
    setExporting(true);
    setExportError(null);
    runAction(runtime.queue, exportCa, undefined, { intent: newIntent() })
      .then((pem) => {
        const file = pem as PemDto;
        return saveCaCertificate(file.filename, file.pem);
      })
      .then(
        (outcome) => {
          setExporting(false);
          if ("saved" in outcome) {
            toast.show({
              kind: "success",
              message: t("security.ca.exported", { path: outcome.saved }),
            });
          }
        },
        (failure: unknown) => {
          setExporting(false);
          setExportError(toForgeApiError(failure, { method: METHOD_TLS_CA_EXPORT }));
        },
      );
  };

  return (
    <RowFrame
      id="ca"
      label={t("security.ca.label")}
      description={t("security.ca.description")}
      caption={CAPTION_FORGE_TLS_CA_STATUS}
    >
      <div className="flex w-full min-w-72 flex-col gap-3">
        {status.error !== null ? (
          <ErrorNotice
            error={status.error}
            context={{ method: METHOD_TLS_CA_STATUS }}
            onRetry={() => {
              void status.refetch();
            }}
          />
        ) : null}
        {ca === undefined ? null : (
          <dl data-testid={TID_CA_STATUS} className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
            <dt className="text-callout text-fg-tertiary">{t("security.ca.state")}</dt>
            <dd>
              <Badge tone={ca.exists ? "success" : "neutral"}>
                {ca.exists ? t("security.ca.exists") : t("security.ca.missing")}
              </Badge>
            </dd>
            {ca.subject === null ? null : (
              <>
                <dt className="text-callout text-fg-tertiary">{t("security.ca.subject")}</dt>
                <dd className="text-callout">{ca.subject}</dd>
              </>
            )}
            {ca.thumbprint_sha256 === null ? null : (
              <>
                <dt className="text-callout text-fg-tertiary">{t("security.ca.thumbprint")}</dt>
                <dd className="font-mono text-mono [overflow-wrap:anywhere]">
                  {ca.thumbprint_sha256}
                </dd>
              </>
            )}
            {ca.not_after === null ? null : (
              <>
                <dt className="text-callout text-fg-tertiary">{t("security.ca.expires")}</dt>
                <dd className="text-callout tabular-nums">{ca.not_after.slice(0, 10)}</dd>
              </>
            )}
            <dt className="text-callout text-fg-tertiary">{t("security.ca.trust")}</dt>
            <dd className="flex flex-wrap gap-1">
              <Badge tone={ca.trusted_current_user ? "success" : "warning"}>
                {ca.trusted_current_user
                  ? t("security.ca.trustedUser")
                  : t("security.ca.notTrustedUser")}
              </Badge>
              <Badge tone={ca.trusted_local_machine ? "success" : "neutral"}>
                {ca.trusted_local_machine
                  ? t("security.ca.trustedMachine")
                  : t("security.ca.notTrustedMachine")}
              </Badge>
            </dd>
            {ca.name_constraints.length === 0 ? null : (
              <>
                <dt className="text-callout text-fg-tertiary">{t("security.ca.constraints")}</dt>
                <dd className="font-mono text-mono">{ca.name_constraints.join(", ")}</dd>
              </>
            )}
          </dl>
        )}

        <section aria-label={t("security.ca.installTitle")} className="flex flex-col gap-2">
          <h3 className="text-callout font-medium">{t("security.ca.installTitle")}</h3>
          <SegmentedControl
            label={t("security.ca.scope")}
            value={scope}
            onValueChange={(next) => {
              setScope(next as TrustScopeDto);
            }}
            options={[
              { value: SCOPE_CURRENT_USER, label: t("security.ca.scopeUser") },
              { value: SCOPE_LOCAL_MACHINE, label: t("security.ca.scopeMachine") },
            ]}
          />
          <p data-testid={TID_CA_INSTALL_NOTE} className="text-callout text-fg-secondary">
            {machine ? t("security.ca.installMachineNote") : t("security.ca.installUserNote")}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <JobButton
              action={installCa}
              input={{ scope }}
              job={install}
              variant="primary"
              testId={TID_CA_INSTALL}
            >
              {t("security.ca.install")}
            </JobButton>
          </div>
          <CliCommand command={installCa.cli({ scope })} />
          <JobStatus
            job={install}
            label={t("security.ca.installing")}
            method={METHOD_TLS_CA_INSTALL}
            testId={TID_CA_INSTALL_STATUS}
            elevated={machine}
          />
        </section>

        <section aria-label={t("security.ca.exportTitle")} className="flex flex-col gap-2">
          <h3 className="text-callout font-medium">{t("security.ca.exportTitle")}</h3>
          <p className="text-callout text-fg-secondary">{t("security.ca.exportNote")}</p>
          <div>
            <Button
              loading={exporting}
              loadingLabel={t("security.ca.exporting")}
              disabled={exportCapability.text !== null || ca?.exists === false}
              data-testid={TID_CA_EXPORT}
              onClick={doExport}
            >
              {t("security.ca.export")}
            </Button>
          </div>
          <CliCommand command={EXPORT_HINT} />
          {exportError === null ? null : (
            <ErrorNotice
              error={exportError}
              context={{ method: METHOD_TLS_CA_EXPORT }}
              onRetry={doExport}
            />
          )}
        </section>

        <section aria-label={t("security.ca.rotateTitle")} className="flex flex-col gap-2">
          <h3 className="text-callout font-medium">{t("security.ca.rotateTitle")}</h3>
          <p className="text-callout text-fg-secondary">{t("security.ca.rotateNote")}</p>
          <RotateCa testId={TID_CA_ROTATE} />
          <CliCommand command="forge tls ca rotate" />
        </section>

        <section aria-label={t("security.ca.uninstallTitle")} className="flex flex-col gap-2">
          <h3 className="text-callout font-medium">{t("security.ca.uninstallTitle")}</h3>
          <p className="text-callout text-fg-secondary">{t("security.ca.uninstallNote")}</p>
          <div className="flex flex-wrap items-center gap-2">
            <JobButton
              action={uninstallCa}
              input={{ scope: "current_user" }}
              job={uninstallUser}
              disabledReason={
                ca?.trusted_current_user === false ? t("security.ca.notInstalledUser") : undefined
              }
              testId={TID_CA_UNINSTALL_USER}
            >
              {t("security.ca.uninstallUser")}
            </JobButton>
            <JobButton
              action={uninstallCa}
              input={{ scope: "local_machine" }}
              job={uninstallMachine}
              disabledReason={
                ca?.trusted_local_machine === false
                  ? t("security.ca.notInstalledMachine")
                  : undefined
              }
              testId={TID_CA_UNINSTALL_MACHINE}
            >
              {t("security.ca.uninstallMachine")}
            </JobButton>
          </div>
          <CliCommand command="forge tls ca uninstall" />
          <JobStatus
            job={uninstallUser}
            label={t("security.ca.uninstalling")}
            method={METHOD_TLS_CA_UNINSTALL}
            testId={TID_CA_UNINSTALL_USER_STATUS}
          />
          <JobStatus
            job={uninstallMachine}
            label={t("security.ca.uninstalling")}
            method={METHOD_TLS_CA_UNINSTALL}
            testId={TID_CA_UNINSTALL_MACHINE_STATUS}
            elevated
          />
        </section>
      </div>
    </RowFrame>
  );
}
