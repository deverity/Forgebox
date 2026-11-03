import { AlertDialog } from "radix-ui";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { removeSite } from "../../actions/sites";
import { runAction } from "../../actions/registry";
import { useCapabilityReason, useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { toast } from "../../design/primitives/toastStore";
import { newIntent } from "../../ipc/api";
import type * as T from "../../ipc/generated";
import { useCapability, useRuntime } from "../../state";

const DIALOG_TESTID = "remove-site-dialog";

export interface RemoveSiteDialogProps {
  /** The site to remove; `null` keeps the dialog closed. */
  site: T.SiteDto | null;
  onClose: () => void;
}

/**
 * Destructive confirmation. It says, before anything runs, that the project files are never
 * deleted; the daemon only accepts `sites.remove` with `confirm: true` (set by the action).
 */
export function RemoveSiteDialog({ site, onClose }: RemoveSiteDialogProps) {
  const { t } = useTranslation("sites");
  const runtime = useRuntime();
  const errorMessage = useErrorMessage();
  const capability = useCapability("sites.remove");
  const reason = useCapabilityReason(capability);
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const close = () => {
    if (running) return;
    setFailure(null);
    onClose();
  };

  const confirm = () => {
    if (site === null || running) return;
    setRunning(true);
    setFailure(null);
    const { id, host } = site;
    runAction(runtime.queue, removeSite, id, { intent: newIntent() }).then(
      () => {
        // The `site.removed` event does the same; this makes the card go away at once.
        runtime.stores.sites.setState((state) => {
          return {
            sites: Object.fromEntries(Object.entries(state.sites).filter(([key]) => key !== id)),
          };
        });
        toast.show({ kind: "success", message: t("remove.done", { host }) });
        setRunning(false);
        onClose();
      },
      (error: unknown) => {
        setFailure(errorMessage(error));
        setRunning(false);
      },
    );
  };

  return (
    <AlertDialog.Root
      open={site !== null}
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <AlertDialog.Content
          data-testid={DIALOG_TESTID}
          className="fixed top-1/2 left-1/2 w-[420px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
        >
          <AlertDialog.Title className="text-headline">
            {t("remove.title", { host: site?.host ?? "" })}
          </AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {t("remove.description")}
          </AlertDialog.Description>
          {failure !== null ? (
            <p role="alert" className="mt-2 text-callout text-state-failed">
              {failure}
            </p>
          ) : null}
          {reason !== null ? <p className="mt-2 text-caption text-fg-tertiary">{reason}</p> : null}
          <div className="mt-4 flex justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary" disabled={running}>
                {t("remove.cancel")}
              </Button>
            </AlertDialog.Cancel>
            <Button
              variant="destructive"
              loading={running}
              loadingLabel={t("remove.removing")}
              disabled={reason !== null}
              onClick={confirm}
            >
              {t("remove.confirm")}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
