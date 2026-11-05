// The one `events.subscribe` of a context (ADR-0034): every event of the daemon, in `seq`
// order, handed to the frame batcher. The bridge already resumes the subscription across
// reconnections from its cursor without loss or duplicates; what reaches here besides
// items is what the stores must react to:
// - `lagged` (events were dropped: slow webview, slow link, daemon-side lag) → resync;
// - `reset` (the daemon's sequence restarted) → cursor back to 0 and resync;
// - `end` (the stream ended by itself) → reopen from the cursor; the daemon replays what
//   followed it, or says `lagged` when it cannot.
import type { ForgeApi } from "../ipc/api";
import type { StreamEntry, StreamHandle } from "../ipc/bridge";
import type * as T from "../ipc/generated";

export type GapReason = "lagged" | "reset";

export interface EventStreamOptions {
  api: ForgeApi;
  /** Events in `seq` order (one call per frame, between gaps). */
  onEvents: (events: T.EventDto[]) => void;
  onGap: (reason: GapReason) => void;
  /** The highest `seq` received changed. */
  onCursor?: (seq: number) => void;
  /** Delays between reopen attempts, ms (the last one repeats). */
  retryMs?: readonly number[];
}

const DEFAULT_RETRY_MS = [500, 1000, 2000, 5000, 10_000];

export class EventStream {
  private readonly options: EventStreamOptions;
  private handle: StreamHandle | null = null;
  private opening: Promise<void> | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private closed = false;
  private seq = 0;

  constructor(options: EventStreamOptions) {
    this.options = options;
  }

  /** Highest `seq` received (0 before any). */
  get cursor(): number {
    return this.seq;
  }

  get open(): boolean {
    return this.handle !== null;
  }

  /** Subscribes (idempotent); failures retry with backoff until `close()`. */
  start(): Promise<void> {
    this.closed = false;
    if (this.handle !== null) return Promise.resolve();
    this.opening ??= this.subscribe().finally(() => {
      this.opening = null;
    });
    return this.opening;
  }

  async close(): Promise<void> {
    this.closed = true;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    const handle = this.handle;
    this.handle = null;
    await handle?.unsubscribe().catch(() => undefined);
  }

  /** Forgets the cursor (the daemon's sequence went backwards). */
  rewind(seq: number): void {
    this.setCursor(seq);
  }

  private async subscribe(): Promise<void> {
    const resumeFrom = this.seq > 0 ? this.seq : null;
    try {
      const handle = await this.options.api.subscribe(
        "events.subscribe",
        { resume_from: resumeFrom },
        (entries) => {
          this.onEntries(entries);
        },
      );
      if (this.closed) {
        await handle.unsubscribe().catch(() => undefined);
        return;
      }
      this.handle = handle;
      this.attempt = 0;
      if (resumeFrom === null && handle.resumePoint !== null && handle.resumePoint > this.seq) {
        this.setCursor(handle.resumePoint);
      }
    } catch (error) {
      console.warn("state: events.subscribe failed; retrying", error);
      this.scheduleRetry();
    }
  }

  private scheduleRetry(): void {
    if (this.closed || this.retryTimer !== null) return;
    const delays = this.options.retryMs ?? DEFAULT_RETRY_MS;
    const delay = delays[Math.min(this.attempt, delays.length - 1)] ?? 1000;
    this.attempt += 1;
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      void this.start();
    }, delay);
  }

  private setCursor(seq: number): void {
    if (seq === this.seq) return;
    this.seq = seq;
    this.options.onCursor?.(seq);
  }

  private onEntries(entries: readonly StreamEntry<T.EventEnvelope>[]): void {
    let events: T.EventDto[] = [];
    let cursor = this.seq;
    const emit = () => {
      if (cursor !== this.seq) this.setCursor(cursor);
      if (events.length > 0) this.options.onEvents(events);
      events = [];
    };
    for (const entry of entries) {
      switch (entry.kind) {
        case "item": {
          const event = entry.item.event;
          if (event.seq <= cursor) break; // Already seen (replays overlap).
          cursor = event.seq;
          events.push(event);
          break;
        }
        case "lagged":
          emit();
          this.options.onGap("lagged");
          break;
        case "reset":
          emit();
          cursor = 0;
          this.setCursor(0);
          this.options.onGap("reset");
          break;
        case "end":
          emit();
          this.handle = null;
          if (entry.error !== null) {
            console.warn(`state: events stream ended: ${entry.error.code}`, entry.error);
          }
          this.scheduleRetry();
          break;
      }
    }
    emit();
  }
}
