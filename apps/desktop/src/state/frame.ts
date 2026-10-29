// Per-frame batching of incoming events (docs/03 §7.5, docs/07 §10): stream frames only
// enqueue; the reducers run once per `requestAnimationFrame` over everything that arrived,
// so 1,000 events in one frame become one store update per store.
//
// - While the page is hidden there is no rAF: batches run on a timer instead, so the queue
//   never grows without bound and the stores are current when the window shows again.
// - Every visibility change flushes at once (the first visible frame paints fresh state).
// - A batch applies at most `maxBatch` items; the rest waits for the next frame, so a storm
//   never makes one long frame. Beyond `maxQueue` queued items the queue is dropped and
//   `onOverflow` runs (the caller resyncs from snapshots, as after a `lagged`).
import type { PageActivity } from "./page";

/** `requestAnimationFrame` / `cancelAnimationFrame`, injectable for tests. */
export interface FrameClock {
  request(callback: () => void): unknown;
  cancel(handle: unknown): void;
}

export function browserFrameClock(): FrameClock {
  if (typeof globalThis.requestAnimationFrame === "function") {
    return {
      request: (callback) =>
        globalThis.requestAnimationFrame(() => {
          callback();
        }),
      cancel: (handle) => {
        globalThis.cancelAnimationFrame(handle as number);
      },
    };
  }
  return {
    request: (callback) => setTimeout(callback, 16),
    cancel: (handle) => {
      clearTimeout(handle as ReturnType<typeof setTimeout>);
    },
  };
}

/** A clock whose frames run only when the test says so. */
export interface ManualFrameClock extends FrameClock {
  /** Runs the callbacks requested so far (one "frame"); returns how many ran. */
  tick(): number;
  pending(): number;
}

export function manualFrameClock(): ManualFrameClock {
  let next = 1;
  const callbacks = new Map<number, () => void>();
  return {
    request(callback) {
      const id = next;
      next += 1;
      callbacks.set(id, callback);
      return id;
    },
    cancel(handle) {
      callbacks.delete(handle as number);
    },
    tick() {
      const due = [...callbacks.values()];
      callbacks.clear();
      for (const callback of due) callback();
      return due.length;
    },
    pending: () => callbacks.size,
  };
}

export const DEFAULT_MAX_BATCH = 2048;
export const DEFAULT_MAX_QUEUE = 50_000;
export const HIDDEN_FLUSH_MS = 250;

export interface FrameBatcherOptions<T> {
  apply: (batch: readonly T[]) => void;
  frame: FrameClock;
  page: PageActivity;
  maxBatch?: number;
  maxQueue?: number;
  /** The queue passed `maxQueue` and was dropped. */
  onOverflow?: () => void;
  hiddenFlushMs?: number;
}

export class FrameBatcher<T> {
  private queue: T[] = [];
  private frameHandle: unknown = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly options: Required<Omit<FrameBatcherOptions<T>, "onOverflow">> &
    Pick<FrameBatcherOptions<T>, "onOverflow">;
  private readonly unsubscribe: () => void;
  private disposed = false;

  constructor(options: FrameBatcherOptions<T>) {
    this.options = {
      maxBatch: DEFAULT_MAX_BATCH,
      maxQueue: DEFAULT_MAX_QUEUE,
      hiddenFlushMs: HIDDEN_FLUSH_MS,
      ...options,
    };
    this.unsubscribe = options.page.subscribe(() => {
      // Shown or hidden: apply what is queued now, then reschedule on the right clock.
      this.flush();
    });
  }

  /** Items waiting for the next batch. */
  get size(): number {
    return this.queue.length;
  }

  push(items: readonly T[]): void {
    if (this.disposed || items.length === 0) return;
    for (const item of items) this.queue.push(item);
    if (this.queue.length > this.options.maxQueue) {
      this.queue = [];
      this.cancelScheduled();
      this.options.onOverflow?.();
      return;
    }
    this.schedule();
  }

  /** Applies everything queued now, in batches of `maxBatch`. */
  flush(): void {
    this.cancelScheduled();
    while (this.queue.length > 0 && !this.disposed) this.runBatch();
  }

  dispose(): void {
    this.disposed = true;
    this.cancelScheduled();
    this.queue = [];
    this.unsubscribe();
  }

  private runBatch(): void {
    const batch = this.queue.splice(0, this.options.maxBatch);
    if (batch.length > 0) this.options.apply(batch);
  }

  private onFrame = (): void => {
    this.frameHandle = null;
    this.timer = null;
    if (this.disposed) return;
    this.runBatch();
    if (this.queue.length > 0) this.schedule();
  };

  private schedule(): void {
    if (this.frameHandle !== null || this.timer !== null) return;
    if (this.options.page.visible()) {
      this.frameHandle = this.options.frame.request(this.onFrame);
    } else {
      this.timer = setTimeout(this.onFrame, this.options.hiddenFlushMs);
    }
  }

  private cancelScheduled(): void {
    if (this.frameHandle !== null) {
      this.options.frame.cancel(this.frameHandle);
      this.frameHandle = null;
    }
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
