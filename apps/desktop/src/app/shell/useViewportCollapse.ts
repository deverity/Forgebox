import { useSyncExternalStore } from "react";

/** The sidebar shows icons only below this window width (docs/07 section 4). */
export const SIDEBAR_COLLAPSE_BELOW_PX = 1100;

function subscribe(listener: () => void): () => void {
  window.addEventListener("resize", listener);
  return () => {
    window.removeEventListener("resize", listener);
  };
}

/** True while the window is narrower than {@link SIDEBAR_COLLAPSE_BELOW_PX}. */
export function useSidebarCollapsed(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => window.innerWidth < SIDEBAR_COLLAPSE_BELOW_PX,
    () => false,
  );
}
