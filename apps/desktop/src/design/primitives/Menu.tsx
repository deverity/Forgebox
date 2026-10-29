import { AnimatePresence, motion } from "motion/react";
import { ContextMenu } from "radix-ui";
import { type ReactNode, useState } from "react";
import { surfaceMotion } from "./useSurfaceMotion";
import { FOCUS_RING } from "./shared";

export type MenuEntry =
  | {
      type?: "item";
      id: string;
      label: string;
      onSelect: () => void;
      shortcut?: string;
      disabled?: boolean;
      destructive?: boolean;
    }
  | { type: "separator"; id: string };

export interface MenuProps {
  entries: readonly MenuEntry[];
  /** The area that opens the menu on right click, long press or the context-menu key. */
  children: ReactNode;
  className?: string;
}

/** Radix context menu (docs/07 section 6) with the popover spring; arrow keys, typeahead, Escape. */
export function Menu({ entries, children, className }: MenuProps) {
  const [open, setOpen] = useState(false);
  const m = surfaceMotion("popover");

  return (
    <ContextMenu.Root onOpenChange={setOpen}>
      <ContextMenu.Trigger asChild>
        {/* Focusable so focus has somewhere to return to when the menu closes. */}
        <div
          role="group"
          // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- focus-return target
          tabIndex={0}
          className={[
            FOCUS_RING,
            className,
          ]
            .filter(Boolean)
            .join(" ")}
        >
          {children}
        </div>
      </ContextMenu.Trigger>
      <AnimatePresence>
        {open ? (
          <ContextMenu.Portal forceMount>
            <ContextMenu.Content asChild forceMount>
              <motion.div
                data-motion={m.dataMotion}
                className="z-50 min-w-48 rounded-sheet border border-stroke bg-surface-2 p-1 text-body text-fg-primary shadow-popover"
                initial={m.initial}
                animate={m.animate}
                exit={m.exit}
                transition={m.transition}
              >
                {entries.map((entry) =>
                  entry.type === "separator" ? (
                    <ContextMenu.Separator key={entry.id} className="my-1 h-px bg-stroke" />
                  ) : (
                    <ContextMenu.Item
                      key={entry.id}
                      disabled={entry.disabled}
                      onSelect={entry.onSelect}
                      className={[
                        "flex cursor-default items-center justify-between gap-6 rounded-control px-2 py-1 outline-none data-[disabled]:text-fg-tertiary data-[highlighted]:bg-accent data-[highlighted]:text-surface",
                        entry.destructive ? "text-state-failed" : "",
                      ]
                        .filter(Boolean)
                        .join(" ")}
                    >
                      <span>{entry.label}</span>
                      {entry.shortcut ? (
                        <kbd className="font-mono text-caption">{entry.shortcut}</kbd>
                      ) : null}
                    </ContextMenu.Item>
                  ),
                )}
              </motion.div>
            </ContextMenu.Content>
          </ContextMenu.Portal>
        ) : null}
      </AnimatePresence>
    </ContextMenu.Root>
  );
}
