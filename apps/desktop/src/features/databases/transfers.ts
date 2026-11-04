// The export / import jobs the Databases screen follows (M2-E3-T14). The list lives outside the
// components so leaving the screen and coming back still shows a transfer in flight: the job
// itself belongs to the daemon (docs/03 §7), this only remembers which ones the user started.
import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";
import { exportDatabase, importDatabase } from "../../actions/databases";
import { startJobAction } from "../../actions/registry";
import { newIntent } from "../../ipc/api";
import type { ContextRuntime } from "../../state";

export type TransferKind = "export" | "import";

/** What a transfer needs to start again ("Tentar de novo"). */
export type TransferInput =
  | { kind: "export"; instance: string; name: string; output?: string | null }
  | { kind: "import"; instance: string; name: string; file: string };

export interface Transfer {
  /** The job id: one row per job. */
  jobId: string;
  context: string;
  input: TransferInput;
}

export interface TransfersState {
  items: readonly Transfer[];
}

export function createTransfersStore(): StoreApi<TransfersState> {
  return createStore<TransfersState>(() => ({ items: [] }));
}

export const transfersStore: StoreApi<TransfersState> = createTransfersStore();

export function useTransfers(): readonly Transfer[] {
  return useStore(transfersStore, (state) => state.items);
}

export function dismissTransfer(
  jobId: string,
  store: StoreApi<TransfersState> = transfersStore,
): void {
  store.setState((state) => ({ items: state.items.filter((item) => item.jobId !== jobId) }));
}

/** Test hook: forgets every transfer. */
export function resetTransfersForTest(store: StoreApi<TransfersState> = transfersStore): void {
  store.setState({ items: [] });
}

/**
 * Starts the export or import job and remembers it. Rejects when the daemon refuses to start it
 * (stopped instance, bad path...); a failure while it runs shows in the row, from the jobs store.
 * `replace` swaps the row of a failed job that is being retried.
 */
export async function startTransfer(
  runtime: ContextRuntime,
  input: TransferInput,
  replace: string | null = null,
  store: StoreApi<TransfersState> = transfersStore,
): Promise<string> {
  const options = { intent: newIntent() };
  const handle =
    input.kind === "export"
      ? await startJobAction(runtime.queue, exportDatabase, input, options)
      : await startJobAction(runtime.queue, importDatabase, input, options);
  // The jobs store shows the outcome; nobody awaits `done` here.
  handle.done.catch(() => undefined);
  const row: Transfer = { jobId: handle.job.id, context: runtime.context, input };
  store.setState((state) => ({
    items: [...state.items.filter((item) => item.jobId !== replace), row],
  }));
  return row.jobId;
}
