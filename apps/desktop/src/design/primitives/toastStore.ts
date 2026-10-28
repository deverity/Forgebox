// Module-level toast store. UI code calls `toast.show(...)` from anywhere (including non-React
// code such as IPC event handlers); <ToastViewport/> subscribes with useSyncExternalStore.

export type ToastKind = "info" | "success" | "error";

export interface ToastUndo {
  /** Defaults to the translated "Undo". */
  label?: string;
  onUndo: () => void;
}

export interface ToastInput {
  kind?: ToastKind;
  message: string;
  undo?: ToastUndo;
  /** Milliseconds before auto-dismiss; 0 keeps it until dismissed. Errors default to sticky. */
  durationMs?: number;
}

export interface ToastItem {
  readonly id: string;
  readonly kind: ToastKind;
  readonly message: string;
  readonly undo?: ToastUndo;
  readonly durationMs: number;
}

export const DEFAULT_DURATION_MS = 5000;
/** Oldest toasts are dropped beyond this many, so the stack never covers the window. */
export const MAX_TOASTS = 4;

let items: readonly ToastItem[] = [];
let counter = 0;
const listeners = new Set<() => void>();

function emit(next: readonly ToastItem[]): void {
  items = next;
  listeners.forEach((l) => {
    l();
  });
}

export const toastStore = {
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  },
  getSnapshot(): readonly ToastItem[] {
    return items;
  },
};

export const toast = {
  show(input: ToastInput): string {
    counter += 1;
    const kind = input.kind ?? "info";
    const item: ToastItem = {
      id: `toast-${String(counter)}`,
      kind,
      message: input.message,
      durationMs: input.durationMs ?? (kind === "error" ? 0 : DEFAULT_DURATION_MS),
      ...(input.undo ? { undo: input.undo } : {}),
    };
    emit([...items, item].slice(-MAX_TOASTS));
    return item.id;
  },
  dismiss(id: string): void {
    if (items.some((i) => i.id === id)) emit(items.filter((i) => i.id !== id));
  },
  clear(): void {
    emit([]);
  },
};
