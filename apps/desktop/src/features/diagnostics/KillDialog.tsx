import { useQuery } from "@tanstack/react-query";
import { AlertDialog } from "radix-ui";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { killPortHolder } from "../../actions/services";
import { useCapabilityReason, useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { toast } from "../../design/primitives/toastStore";
import { newIntent } from "../../ipc/api";
import { queryKeys, useCapability, useForgeApi, useRuntime } from "../../state";

const DIALOG_TESTID = "kill-port-dialog";
const HOLDER_TESTID = "kill-holder";

export interface KillDialogProps {
  /** The port whose holder would be ended; `null` keeps the dialog closed. */
  port: number | null;
  onClose: () => void;
  onKilled: () => void;
}

/**
 * Confirmation of "Encerrar": the holder is read again with `net.port_check` when the dialog
 * opens, so the pid and exe shown are the ones `services.kill_port_holder` will receive (the
 * daemon refuses a pid that no longer holds the port). Local daemons only.
 */
export function KillDialog({ port, onClose, onKilled }: KillDialogProps) {
  const { t } = useTranslation("diagnostics");
  const api = useForgeApi();
  const runtime = useRuntime();
  const errorMessage = useErrorMessage();
  const capability = useCapability(killPortHolder.method);
  const reason = useCapabilityReason(capability);
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const holder = useQuery({
    queryKey: [...queryKeys.call(api.context, "net.port_check", { port: port ?? 0 }), "kill"],
    queryFn: () => api.call("net.port_check", { port: port ?? 0 }),
    enabled: port !== null,
    staleTime: 0,
    gcTime: 0,
  });
  const found = holder.data?.holder ?? null;
  const close = () => {
    if (running) return;
    setFailure(null);
    onClose();
  };
  const confirm = () => {
    if (port === null || found === null || running) return;
    setRunning(true);
    setFailure(null);
    runAction(
      runtime.queue,
      killPortHolder,
      { port, pid: found.pid },
      { intent: newIntent() },
    ).then(
      () => {
        toast.show({ kind: "success", message: t("kill.done", { port }) });
        setRunning(false);
        onClose();
        onKilled();
      },
      (error: unknown) => {
        setFailure(errorMessage(error));
        setRunning(false);
      },
    );
  };
  return (
    <AlertDialog.Root
      open={port !== null}
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <AlertDialog.Content
          data-testid={DIALOG_TESTID}
          className="fixed top-1/2 left-1/2 w-[460px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
        >
          <AlertDialog.Title className="text-headline">
            {t("kill.title", { port: port ?? 0 })}
          </AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {holder.isPending
              ? t("kill.loading")
              : holder.isError
                ? t("kill.lookupFailed")
                : found === null
                  ? t("kill.free", { port: port ?? 0 })
                  : t("kill.description")}
          </AlertDialog.Description>
          {found === null ? null : (
            <dl
              data-testid={HOLDER_TESTID}
              className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1"
            >
              <dt className="text-caption text-fg-tertiary">{t("kill.pid")}</dt>
              <dd className="font-mono text-mono tabular-nums">{found.pid}</dd>
              <dt className="text-caption text-fg-tertiary">{t("kill.exe")}</dt>
              <dd className="font-mono text-mono [overflow-wrap:anywhere]">{found.exe}</dd>
            </dl>
          )}
          {port === null || found === null ? null : (
            <div className="mt-3">
              <CliCommand command={killPortHolder.cli({ port, pid: found.pid })} />
            </div>
          )}
          {failure !== null ? (
            <p role="alert" className="mt-2 text-callout text-state-failed">
              {failure}
            </p>
          ) : null}
          {reason !== null ? <p className="mt-2 text-caption text-fg-tertiary">{reason}</p> : null}
          <div className="mt-4 flex justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary" disabled={running}>
                {t("kill.cancel")}
              </Button>
            </AlertDialog.Cancel>
            <Button
              variant="destructive"
              loading={running}
              loadingLabel={t("kill.killing")}
              disabled={reason !== null || found === null}
              onClick={confirm}
            >
              {t("kill.confirm")}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
