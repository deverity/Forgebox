import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { toast } from "../../design/primitives/toastStore";
import type * as T from "../../ipc/generated";
import { saveTextFile } from "../../ipc/native";
import { useForgeApi, useHello } from "../../state";
import { bundleFileName, bundleJson, buildBundle, EXPORT_LOG_LINES } from "./model";

export interface ExportButtonProps {
  checks: readonly T.CheckDto[];
  disabled: boolean;
  onError: (message: string | null) => void;
}

/**
 * "Exportar relatorio": builds the support bundle of `forge doctor --export` on the client from
 * public methods only (so it also works against a remote daemon) and saves it through the dialog.
 * A daemon log that cannot be read does not fail the export (the CLI's rule).
 */
export function ExportButton({ checks, disabled, onError }: ExportButtonProps) {
  const { t } = useTranslation("diagnostics");
  const api = useForgeApi();
  const hello = useHello();
  const errorMessage = useErrorMessage();
  const [busy, setBusy] = useState(false);
  const run = async () => {
    if (hello === null) return;
    setBusy(true);
    onError(null);
    try {
      const [status, tools] = await Promise.all([
        api.call("daemon.status", {}),
        api.call("tools.list", { installed: true, updates: false }),
      ]);
      let log: T.LogLineDto[] | null = null;
      let logError: string | undefined;
      try {
        log = (await api.call("logs.read", { target: "daemon", limit: EXPORT_LOG_LINES })).lines;
      } catch (error) {
        logError = error instanceof Error ? error.message : String(error);
      }
      const now = Math.floor(Date.now() / 1000);
      const bundle = buildBundle({
        checks,
        status,
        tools: tools.tools,
        log,
        ...(logError === undefined ? {} : { logError }),
        version: hello.daemon_version,
        apiVersion: hello.api_version,
        now,
      });
      const outcome = await saveTextFile(bundleFileName(now), bundleJson(bundle));
      if ("saved" in outcome) {
        toast.show({ kind: "success", message: t("export.saved", { path: outcome.saved }) });
      }
    } catch (error) {
      onError(t("export.failed", { cause: errorMessage(error) }));
    } finally {
      setBusy(false);
    }
  };
  return (
    // R-31: the reason a disabled button is disabled is a tooltip on its wrapper (a disabled
    // button gets no pointer events) and a sentence on the idle screen.
    <span title={disabled ? t("export.needsRun") : undefined} className="inline-flex">
      <Button
        variant="secondary"
        loading={busy}
        loadingLabel={t("export.exporting")}
        disabled={disabled || hello === null}
        onClick={() => {
          void run();
        }}
      >
        {t("export.action")}
      </Button>
    </span>
  );
}
