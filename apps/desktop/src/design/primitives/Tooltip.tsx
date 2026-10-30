import { AnimatePresence, motion } from "motion/react";
import { Tooltip as RadixTooltip } from "radix-ui";
import { type ReactNode, useState } from "react";
import { surfaceMotion } from "./useSurfaceMotion";

export interface TooltipProps {
  content: ReactNode;
  /** Equivalent CLI command, shown in a mono slot (docs/07 section 11). Never translated. */
  command?: string;
  children: ReactNode;
  delayMs?: number;
}

/** Radix tooltip with the popover spring and a slot for the equivalent `forge` command. */
export function Tooltip({ content, command, children, delayMs = 400 }: TooltipProps) {
  const [open, setOpen] = useState(false);
  const m = surfaceMotion("popover");

  return (
    <RadixTooltip.Provider delayDuration={delayMs}>
      <RadixTooltip.Root open={open} onOpenChange={setOpen}>
        <RadixTooltip.Trigger asChild>{children}</RadixTooltip.Trigger>
        <AnimatePresence>
          {open ? (
            <RadixTooltip.Portal forceMount>
              <RadixTooltip.Content asChild forceMount sideOffset={6}>
                <motion.div
                  data-motion={m.dataMotion}
                  className="z-50 max-w-72 rounded-control border border-stroke bg-surface-2 px-2 py-1 text-callout text-fg-primary shadow-popover"
                  initial={m.initial}
                  animate={m.animate}
                  exit={m.exit}
                  transition={m.transition}
                >
                  <div>{content}</div>
                  {command ? (
                    <code
                      data-slot="command"
                      data-forge-cli={command}
                      className="mt-1 block font-mono text-mono text-fg-secondary"
                    >
                      {command}
                    </code>
                  ) : null}
                </motion.div>
              </RadixTooltip.Content>
            </RadixTooltip.Portal>
          ) : null}
        </AnimatePresence>
      </RadixTooltip.Root>
    </RadixTooltip.Provider>
  );
}
