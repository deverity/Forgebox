import type { ReactNode } from "react";

/** Keyboard key cap, e.g. <Kbd>Ctrl</Kbd>+<Kbd>K</Kbd>. */
export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-5 min-w-5 items-center justify-center rounded-control border border-stroke-strong bg-surface-2 px-1 font-mono text-caption text-fg-secondary">
      {children}
    </kbd>
  );
}
