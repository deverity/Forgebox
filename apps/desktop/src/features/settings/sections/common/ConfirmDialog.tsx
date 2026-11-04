import { AlertDialog } from "radix-ui";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../../../design/primitives/Button";
import { CliCommand } from "../../../../design/primitives/CliCommand";

export interface ConfirmDialogProps {
  open: boolean;
  /** Cancel, Escape and click-away all end up here with `false`. */
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  confirmLabel: string;
  onConfirm: () => void;
  destructive?: boolean;
  /** The equivalent `forge` command shown under the text. */
  command?: string | null;
  testId: string;
  children?: ReactNode;
}

/** Confirmation modal (Radix AlertDialog: focus trap, Escape cancels, focus returns). */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  onConfirm,
  destructive = false,
  command,
  testId,
  children,
}: ConfirmDialogProps) {
  const { t } = useTranslation("settings");
  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <AlertDialog.Content
          data-testid={testId}
          className="fixed top-1/2 left-1/2 w-[460px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
        >
          <AlertDialog.Title className="text-headline">{title}</AlertDialog.Title>
          <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
            {description}
          </AlertDialog.Description>
          {children}
          {command === undefined || command === null ? null : (
            <div className="mt-3">
              <CliCommand command={command} />
            </div>
          )}
          <div className="mt-4 flex justify-end gap-2">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary">{t("confirm.cancel")}</Button>
            </AlertDialog.Cancel>
            <Button
              variant={destructive ? "destructive" : "primary"}
              onClick={() => {
                onConfirm();
              }}
            >
              {confirmLabel}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
