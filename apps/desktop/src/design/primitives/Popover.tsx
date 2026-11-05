import { AnimatePresence, motion } from "motion/react";
import { Popover as RadixPopover } from "radix-ui";
import { type ReactNode, useState } from "react";
import { surfaceMotion } from "./useSurfaceMotion";

export interface PopoverProps {
  trigger: ReactNode;
  children: ReactNode;
  /** Accessible name of the dialog. */
  label: string;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  align?: "start" | "center" | "end";
}

/**
 * Floating panel with the 260/26 popover spring and 12 px radius. Modal on purpose: focus is
 * trapped while open, Escape closes and focus returns to the trigger.
 */
export function Popover({
  trigger,
  children,
  label,
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  align = "start",
}: PopoverProps) {
  const [inner, setInner] = useState(defaultOpen);
  const open = openProp ?? inner;
  const change = (next: boolean) => {
    setInner(next);
    onOpenChange?.(next);
  };
  const m = surfaceMotion("popover");

  return (
    <RadixPopover.Root open={open} onOpenChange={change} modal>
      <RadixPopover.Trigger asChild>{trigger}</RadixPopover.Trigger>
      <AnimatePresence>
        {open ? (
          <RadixPopover.Portal forceMount>
            <RadixPopover.Content
              asChild
              forceMount
              align={align}
              sideOffset={8}
              aria-label={label}
            >
              <motion.div
                data-motion={m.dataMotion}
                className="z-50 rounded-sheet border border-stroke bg-surface-2 p-3 text-body text-fg-primary shadow-popover"
                initial={m.initial}
                animate={m.animate}
                exit={m.exit}
                transition={m.transition}
              >
                {children}
              </motion.div>
            </RadixPopover.Content>
          </RadixPopover.Portal>
        ) : null}
      </AnimatePresence>
    </RadixPopover.Root>
  );
}
