import { AlertDialog } from "radix-ui";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { dropDatabase } from "../../actions/databases";
import { runAction } from "../../actions/registry";
import { useCapabilityReason, useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { TextField } from "../../design/primitives/TextField";
import { toast } from "../../design/primitives/toastStore";
import { newIntent } from "../../ipc/api";
import { useCapability, useRuntime } from "../../state";

const DIALOG_TESTID = "drop-database-dialog";

export interface DropTarget {
  instance: string;
  name: string;
}

/**
 * Destructive confirmation: the database name has to be typed exactly (case-sensitive) before
 * the button works, and only then does `db.drop` go out with `confirm: true`.
 */
export function DropDialog({
  target,
  onClose,
}: {
  target: DropTarget | null;
  onClose: () => void;
}) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const errorMessage = useErrorMessage();
  const capability = useCapability(dropDatabase.method);
  const reason = useCapabilityReason(capability);
  const [typed, setTyped] = useState("");
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const close = () => {
    if (running) return;
    setTyped("");
    setFailure(null);
    onClose();
  };
  const confirm = () => {
    if (target === null || running || typed !== target.name) return;
    setRunning(true);
    setFailure(null);
    const { name } = target;
    runAction(runtime.queue, dropDatabase, target, { intent: newIntent() }).then(
      () => {
        toast.show({ kind: "success", message: t("drop.done", { name }) });
        setRunning(false);
        setTyped("");
        onClose();
      },
      (error: unknown) => {
        setFailure(errorMessage(error));
        setRunning(false);
      },
    );
  };
  const name = target?.name ?? "";
  return (
    <AlertDialog.Root
      open={target !== null}
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
            {t("drop.title", { name })}
          </AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {t("drop.description")}
          </AlertDialog.Description>
          <div className="mt-3 flex flex-col gap-3">
            <TextField
              label={t("drop.confirmLabel", { name })}
              value={typed}
              width="full"
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => {
                setTyped(event.target.value);
              }}
            />
            {target === null ? null : <CliCommand command={dropDatabase.cli(target)} />}
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
                {t("drop.cancel")}
              </Button>
            </AlertDialog.Cancel>
            <Button
              variant="destructive"
              loading={running}
              loadingLabel={t("drop.dropping")}
              disabled={reason !== null || typed !== name || name === ""}
              onClick={confirm}
            >
              {t("drop.confirm")}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
