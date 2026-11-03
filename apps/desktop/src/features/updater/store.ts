// One shared view of the updater state for the sidebar footer and the settings panel.
// The store loads lazily on the first subscriber, after first paint (never blocks it), and
// follows the `forge://updater` event afterwards.
import { useSyncExternalStore } from "react";
import { INACTIVE_SNAPSHOT, defaultUpdaterApi, type UpdaterApi, type UpdaterSnapshot } from "./api";

export interface UpdaterStore {
  readonly api: UpdaterApi;
  subscribe: (listener: () => void) => () => void;
  getSnapshot: () => UpdaterSnapshot;
  /** Asks the shell for the state now (also used after an action). */
  refresh: () => Promise<void>;
}

export function createUpdaterStore(api: UpdaterApi): UpdaterStore {
  let snapshot = INACTIVE_SNAPSHOT;
  const listeners = new Set<() => void>();
  let stopEvents: (() => void) | null = null;
  let starting = false;

  const publish = (next: UpdaterSnapshot) => {
    snapshot = next;
    listeners.forEach((listener) => {
      listener();
    });
  };

  const refresh = async () => {
    try {
      publish(await api.status());
    } catch (error) {
      console.warn("updater: status unavailable", error);
    }
  };

  const start = () => {
    if (starting) return;
    starting = true;
    // Listen first, then ask: an event between the two is never lost.
    api.subscribe(publish).then(
      (stop) => {
        stopEvents = stop;
      },
      (error: unknown) => {
        console.warn("updater: events unavailable", error);
      },
    );
    void refresh();
  };

  return {
    api,
    subscribe: (listener) => {
      listeners.add(listener);
      start();
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0 && stopEvents) {
          stopEvents();
          stopEvents = null;
          starting = false;
        }
      };
    },
    getSnapshot: () => snapshot,
    refresh,
  };
}

let shared: UpdaterStore | null = null;

export function getUpdaterStore(): UpdaterStore {
  shared ??= createUpdaterStore(defaultUpdaterApi());
  return shared;
}

/** Test hook: replace (or reset with `null`) the shared store. */
export function setUpdaterStoreForTests(store: UpdaterStore | null): void {
  shared = store;
}

export function useUpdaterSnapshot(): UpdaterSnapshot {
  const store = getUpdaterStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot);
}
