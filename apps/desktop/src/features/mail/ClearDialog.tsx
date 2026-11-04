import { AlertDialog } from "radix-ui";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { clearMail } from "../../actions/mail";
import { useCapability, useRuntime } from "../../state";
import { useCapabilityReason, useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { toast } from "../../design/primitives/toastStore";
import { newIntent } from "../../ipc/api";

const DIALOG_TESTID = "mail-clear-dialog";

export interface ClearDialogProps {
  open: boolean;
  count: number;
  onClose: () => void;
  /** The mailbox was emptied. */
  onCleared: () => void;
}

/**
 * Destructive confirmation of `mail.clear`. The command shown carries `--yes`, the flag the CLI
 * needs to skip its own `[s/N]` prompt (docs/09); the daemon requires `confirm: true`, which the
 * action sets after this dialog.
 */
export function ClearDialog({ open, count, onClose, onCleared }: ClearDialogProps) {
  const { t } = useTranslation("mail");
  const runtime = useRuntime();
  const errorMessage = useErrorMessage();
  const capability = useCapability("mail.clear");
  const reason = useCapabilityReason(capability);
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const close = () => {
    if (running) return;
    setFailure(null);
    onClose();
  };

  const confirm = () => {
    if (running) return;
    setRunning(true);
    setFailure(null);
    runAction(runtime.queue, clearMail, undefined, { intent: newIntent() }).then(
      () => {
        toast.show({ kind: "success", message: t("clear.done") });
        setRunning(false);
        onCleared();
      },
      (error: unknown) => {
        setFailure(errorMessage(error));
        setRunning(false);
      },
    );
  };

  const command = clearMail.cli(undefined);
  return (
    <AlertDialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <AlertDialog.Content
          data-testid={DIALOG_TESTID}
          className="fixed top-1/2 left-1/2 w-[440px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
        >
          <AlertDialog.Title className="text-headline">{t("clear.title")}</AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {t("clear.description", { count })}
          </AlertDialog.Description>
          <div className="mt-3">
            <CliCommand command={command === null ? null : `${command} --yes`} />
          </div>
          {failure !== null ? (
            <p role="alert" className="mt-2 text-callout text-state-failed">
              {failure}
            </p>
          ) : null}
          {reason !== null ? <p className="mt-2 text-caption text-fg-tertiary">{reason}</p> : null}
          <div className="mt-4 flex justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary" disabled={running}>
                {t("clear.cancel")}
              </Button>
            </AlertDialog.Cancel>
            <Button
              variant="destructive"
              loading={running}
              loadingLabel={t("clear.clearing")}
              disabled={reason !== null}
              onClick={confirm}
            >
              {t("clear.confirm")}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
