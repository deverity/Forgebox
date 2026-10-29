// Derived values the shell chrome reads from the state layer. Selectors return primitives, so a
// service event re-renders only the badge that changed, and nothing remounts (M2-E1-T10 #1).
import { useStore } from "zustand";
import { useCapability, useForgeQuery, useRuntime, useToolUpdates } from "../../state";

export interface ServiceCounts {
  running: number;
  stopped: number;
}

/** Instances running vs stopped (failed counts as stopped: it is not running). */
export function useServiceCounts(): ServiceCounts {
  const stores = useRuntime().stores;
  const running = useStore(stores.services, (s) =>
    Object.values(s.instances).reduce((n, i) => n + (i.state === "running" ? 1 : 0), 0),
  );
  const stopped = useStore(stores.services, (s) =>
    Object.values(s.instances).reduce(
      (n, i) => n + (i.state === "stopped" || i.state === "failed" ? 1 : 0),
      0,
    ),
  );
  return { running, stopped };
}

export function useSiteCount(): number {
  return useStore(useRuntime().stores.sites, (s) => Object.keys(s.sites).length);
}

export function useUpdateCount(): number {
  return useToolUpdates().count;
}

/**
 * Unread mail. `null` hides the badge: `mail.status` is not implemented (or not allowed) on
 * this daemon, or nothing is unread.
 */
export function useMailUnread(): number | null {
  const capability = useCapability("mail.status");
  const status = useForgeQuery("mail.status", {}, { enabled: capability.available });
  if (!capability.available) return null;
  const unread = status.data?.unread ?? 0;
  return unread > 0 ? unread : null;
}
