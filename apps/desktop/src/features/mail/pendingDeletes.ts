// Deferred deletes of the Mail screen: "Apagar" hides the message and shows an undo toast; the
// `mail.delete` call only goes out when the window ends (docs/07 section 11: undo before the
// call, never a call that has to be reverted). The state is module-level so the window keeps
// running if the user leaves the screen, and so does the undo.
import { createStore } from "zustand/vanilla";

/** How long the user can undo, in ms. */
export const DELETE_UNDO_MS = 5000;

const store = createStore<{ ids: ReadonlySet<string> }>(() => ({ ids: new Set() }));
const timers = new Map<string, ReturnType<typeof setTimeout>>();

export const pendingDeletes = store;

function setIds(update: (ids: Set<string>) => void): void {
  const next = new Set(store.getState().ids);
  update(next);
  store.setState({ ids: next });
}

/**
 * Hides `id` and runs `commit` after the undo window. `settled` runs after `commit` ends
 * (success or failure) so the caller can show the message again on failure; the message is
 * unhidden either way once the mailbox refetches.
 */
export function scheduleDelete(
  id: string,
  commit: () => Promise<void>,
  settled: (ok: boolean) => void,
  ms: number = DELETE_UNDO_MS,
): void {
  cancelDelete(id);
  setIds((ids) => ids.add(id));
  timers.set(
    id,
    setTimeout(() => {
      timers.delete(id);
      commit().then(
        () => {
          setIds((ids) => ids.delete(id));
          settled(true);
        },
        () => {
          setIds((ids) => ids.delete(id));
          settled(false);
        },
      );
    }, ms),
  );
}

/** Undo: the call never goes out and the message is listed again. */
export function cancelDelete(id: string): boolean {
  const timer = timers.get(id);
  if (timer === undefined) return false;
  clearTimeout(timer);
  timers.delete(id);
  setIds((ids) => ids.delete(id));
  return true;
}

/** Test helper. */
export function resetPendingDeletes(): void {
  for (const timer of timers.values()) clearTimeout(timer);
  timers.clear();
  store.setState({ ids: new Set() });
}
