import { AnimatePresence, motion } from "motion/react";
import { Dialog } from "radix-ui";
import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";
import { surfaceMotion } from "./useSurfaceMotion";

const TIMES = "×";

export interface SheetProps {
  /** Element that opens the sheet; receives focus back when the sheet closes. */
  trigger?: ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  title: string;
  description?: string;
  children: ReactNode;
}

/**
 * Side panel, 420 px wide (docs/07 section 6). Radix Dialog supplies the focus trap, Escape to
 * close and focus return; Motion supplies the 280 ms enter/exit (100 ms fade under reduced motion).
 */
export function Sheet({
  trigger,
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  title,
  description,
  children,
}: SheetProps) {
  const { t } = useTranslation();
  const [inner, setInner] = useState(defaultOpen);
  const open = openProp ?? inner;
  const change = (next: boolean) => {
    setInner(next);
    onOpenChange?.(next);
  };
  const m = surfaceMotion("sheet");

  return (
    <Dialog.Root open={open} onOpenChange={change}>
      {trigger ? <Dialog.Trigger asChild>{trigger}</Dialog.Trigger> : null}
      <AnimatePresence>
        {open ? (
          <Dialog.Portal forceMount>
            <Dialog.Overlay asChild forceMount>
              <motion.div
                className="fixed inset-0 bg-fg-primary/20"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={m.transition}
              />
            </Dialog.Overlay>
            <Dialog.Content asChild forceMount>
              <motion.aside
                data-motion={m.dataMotion}
                className="fixed top-0 right-0 bottom-0 flex w-[420px] max-w-full flex-col border-l border-stroke bg-surface-2 text-fg-primary shadow-popover"
                initial={m.initial}
                animate={m.animate}
                exit={m.exit}
                transition={m.transition}
              >
                <header className="flex items-start justify-between gap-3 border-b border-stroke p-4">
                  <div className="min-w-0">
                    <Dialog.Title className="text-headline">{title}</Dialog.Title>
                    <Dialog.Description
                      className={description ? "text-callout text-fg-secondary" : "sr-only"}
                    >
                      {description ?? title}
                    </Dialog.Description>
                  </div>
                  <Dialog.Close
                    aria-label={t("ui.sheetClose")}
                    className="rounded-control px-2 py-1 text-fg-secondary hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent"
                  >
                    <span aria-hidden="true">{TIMES}</span>
                  </Dialog.Close>
                </header>
                <div className="min-h-0 flex-1 overflow-auto p-4">{children}</div>
              </motion.aside>
            </Dialog.Content>
          </Dialog.Portal>
        ) : null}
      </AnimatePresence>
    </Dialog.Root>
  );
}
