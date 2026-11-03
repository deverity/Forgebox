// One terminal session of the GUI (docs/07 §5.10, ADR-0040): the glue between an emulator
// (`TermPort`, xterm in the app, a fake in tests) and the `terminal.*` API of the daemon.
//
// - open: `terminal.open` with the cols/rows the fit addon measured;
// - output: `terminal.subscribe` entries are queued and written to the emulator in chunks, the
//   next chunk only after the write callback of the previous one (flow control: the emulator
//   parses at its own pace and the queue absorbs bursts);
// - input: `terminal.input` with the `writer` of the tab and a strictly increasing `input_seq`,
//   one request in flight; a failed request is retried with the same `input_seq` (the daemon
//   deduplicates) and keystrokes typed meanwhile are batched into the next one;
// - resize: debounced (50 ms);
// - lagged/reset: the output is incomplete, so the emulator is reset and the whole ring is
//   replayed (`resume_from: null`);
// - exit: once the queue is written, `onExit(code)` lets the screen show the exit banner.
import type { StreamEntry, StreamHandle } from "../../ipc/bridge";
import type * as T from "../../ipc/generated";

/** What the session needs from the emulator. */
export interface TermPort {
  readonly cols: number;
  readonly rows: number;
  write(data: string, callback?: () => void): void;
  reset(): void;
}

/** What the session needs from the API (actions run the mutating calls). */
export interface SessionApi {
  open(params: T.TerminalOpenParams): Promise<T.TerminalDto>;
  input(params: T.TerminalInputParams): Promise<unknown>;
  resize(params: T.TerminalResizeParams): Promise<unknown>;
  subscribe(
    params: T.TerminalSubscribeParams,
    onEntries: (entries: StreamEntry<T.TerminalEnvelope>[]) => void,
  ): Promise<StreamHandle>;
}

export interface SessionCallbacks {
  onOpened?: (dto: T.TerminalDto) => void;
  onExit?: (code: number | null) => void;
  onError?: (error: unknown) => void;
}

export interface SessionOptions {
  /** `writer` of `terminal.input`: one ULID per tab, reused across reconnects and reopening. */
  writer: string;
  term: TermPort;
  api: SessionApi;
  /** Measures the container of the emulator: the size `terminal.open` and `resize` use. */
  fit: () => { cols: number; rows: number };
  callbacks?: SessionCallbacks;
  resizeDebounceMs?: number;
  /** Largest string handed to one `term.write`. */
  chunkChars?: number;
  retryDelayMs?: number;
  maxInputRetries?: number;
}

export const RESIZE_DEBOUNCE_MS = 50;
export const WRITE_CHUNK_CHARS = 64 * 1024;

function isNotFound(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { code?: unknown }).code === "not_found"
  );
}

export class TerminalSession {
  private readonly writer: string;
  private readonly term: TermPort;
  private readonly api: SessionApi;
  private readonly fit: () => { cols: number; rows: number };
  private readonly callbacks: SessionCallbacks;
  private readonly resizeMs: number;
  private readonly chunk: number;
  private readonly retryMs: number;
  private readonly maxRetries: number;

  private id: string | null = null;
  private handle: StreamHandle | null = null;
  /** Bumped on every (re)subscription and replay: entries of older streams are ignored. */
  private generation = 0;
  private disposed = false;
  private seq = 0;

  // output
  private queue: string[] = [];
  private writing = false;
  /** Bumped on reset so a write callback of discarded output does not advance the new queue. */
  private writeEpoch = 0;
  private exitCode: number | null | undefined;
  private exitReported = false;

  // input
  private pendingInput = "";
  private inputSeq = 0;
  private draining = false;

  // resize
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;
  private resizeTo: { cols: number; rows: number } | null = null;

  private endRetries = 0;

  constructor(options: SessionOptions) {
    this.writer = options.writer;
    this.term = options.term;
    this.api = options.api;
    this.fit = options.fit;
    this.callbacks = options.callbacks ?? {};
    this.resizeMs = options.resizeDebounceMs ?? RESIZE_DEBOUNCE_MS;
    this.chunk = options.chunkChars ?? WRITE_CHUNK_CHARS;
    this.retryMs = options.retryDelayMs ?? 250;
    this.maxRetries = options.maxInputRetries ?? 4;
  }

  /** Daemon id of the session once opened or attached. */
  get terminalId(): string | null {
    return this.id;
  }

  /** Offset just past the last output received (the `resume_from` of a reattachment). */
  get lastSeq(): number {
    return this.seq;
  }

  /** Opens a new session sized like the emulator (after the fit addon ran) and streams it. */
  async open(params: T.TerminalOpenParams = {}): Promise<T.TerminalDto> {
    const { cols, rows } = this.fit();
    const dto = await this.api.open({ ...params, cols, rows });
    if (this.disposed) return dto;
    this.id = dto.id;
    this.exitCode = undefined;
    this.exitReported = false;
    this.callbacks.onOpened?.(dto);
    await this.start(null);
    void this.drainInput();
    return dto;
  }

  /**
   * Attaches to a session the daemon already hosts (reload, reconnect). The stream resumes
   * from `resumeFrom` (the listed `last_seq`); a resize right after makes the pseudo-console
   * repaint what a fresh emulator does not have.
   */
  async attach(id: string, resumeFrom: number | null): Promise<void> {
    this.id = id;
    this.exitCode = undefined;
    this.exitReported = false;
    if (resumeFrom !== null) this.seq = resumeFrom;
    await this.start(resumeFrom);
    const { cols, rows } = this.fit();
    this.flushResize({ cols, rows });
    void this.drainInput();
  }

  /** Forgets the stream and the screen, for "Reabrir" (a new `open` follows). */
  resetForReopen(): void {
    this.generation += 1;
    this.dropHandle();
    this.id = null;
    this.seq = 0;
    this.clearOutput();
    this.exitCode = undefined;
    this.exitReported = false;
    this.pendingInput = "";
  }

  // --- output ----------------------------------------------------------------------------

  private dropHandle(): void {
    const old = this.handle;
    this.handle = null;
    if (old) void old.unsubscribe().catch(() => undefined);
  }

  private async start(resumeFrom: number | null): Promise<void> {
    const id = this.id;
    if (id === null || this.disposed) return;
    this.generation += 1;
    const generation = this.generation;
    this.dropHandle();
    try {
      const handle = await this.api.subscribe({ id, resume_from: resumeFrom }, (entries) => {
        this.onEntries(generation, entries);
      });
      if (!this.isCurrent(generation)) {
        void handle.unsubscribe().catch(() => undefined);
        return;
      }
      this.handle = handle;
    } catch (error) {
      if (this.isCurrent(generation)) this.callbacks.onError?.(error);
    }
  }

  /** `false` once a newer subscription, a replay or `dispose` superseded `generation`. */
  private isCurrent(generation: number): boolean {
    return generation === this.generation && !this.disposed;
  }

  private onEntries(generation: number, entries: StreamEntry<T.TerminalEnvelope>[]): void {
    for (const entry of entries) {
      // A replay bumps the generation mid-frame: the rest of the frame belongs to the old stream.
      if (!this.isCurrent(generation)) return;
      switch (entry.kind) {
        case "item": {
          const { seq, item } = entry.item;
          this.endRetries = 0;
          if (item.kind === "output") {
            this.seq = seq;
            this.queue.push(item.data);
            this.pump();
          } else {
            this.exitCode = item.exit_code;
            this.reportExitWhenDrained();
          }
          break;
        }
        case "lagged":
        case "reset":
          this.replay();
          return;
        case "end":
          this.onEnd(entry.error);
          return;
      }
    }
  }

  /** Output was lost: clear the screen and ask for the whole ring again. */
  private replay(): void {
    this.generation += 1;
    this.dropHandle();
    this.clearOutput();
    this.seq = 0;
    void this.start(null);
  }

  private clearOutput(): void {
    this.queue = [];
    this.writing = false;
    this.writeEpoch += 1;
    this.term.reset();
  }

  private onEnd(error: unknown): void {
    this.handle = null;
    if (this.exitCode !== undefined) return;
    if (isNotFound(error)) {
      this.exitCode = null;
      this.reportExitWhenDrained();
      return;
    }
    if (error !== null && error !== undefined) this.callbacks.onError?.(error);
    if (this.endRetries >= 5 || this.disposed) return;
    this.endRetries += 1;
    const generation = this.generation;
    setTimeout(() => {
      if (this.isCurrent(generation)) void this.start(this.seq);
    }, 1000);
  }

  /** Writes queued output one chunk at a time, each after the previous write callback. */
  private pump(): void {
    if (this.writing) return;
    const first = this.queue.shift();
    if (first === undefined) {
      this.reportExitWhenDrained();
      return;
    }
    let data = first;
    // Join small items up to one chunk; split a huge one.
    while (data.length < this.chunk && this.queue.length > 0) {
      const next = this.queue[0];
      if (next === undefined || data.length + next.length > this.chunk) break;
      data += next;
      this.queue.shift();
    }
    if (data.length > this.chunk) {
      this.queue.unshift(data.slice(this.chunk));
      data = data.slice(0, this.chunk);
    }
    this.writing = true;
    const epoch = this.writeEpoch;
    this.term.write(data, () => {
      if (epoch !== this.writeEpoch) return;
      this.writing = false;
      this.pump();
    });
  }

  private reportExitWhenDrained(): void {
    if (this.exitCode === undefined || this.exitReported) return;
    if (this.writing || this.queue.length > 0) return;
    this.exitReported = true;
    this.callbacks.onExit?.(this.exitCode);
  }

  // --- input -----------------------------------------------------------------------------

  /** Keystrokes from the emulator (or pasted text). */
  sendInput(data: string): void {
    if (this.disposed || this.exitCode !== undefined || data === "") return;
    this.pendingInput += data;
    void this.drainInput();
  }

  private async drainInput(): Promise<void> {
    if (this.draining) return;
    this.draining = true;
    try {
      while (this.pendingInput !== "" && !this.disposed) {
        const id = this.id;
        if (id === null) return; // Typed before the session exists: sent right after `open`.
        const data = this.pendingInput;
        this.pendingInput = "";
        this.inputSeq += 1;
        await this.sendWithRetry({ id, writer: this.writer, data, input_seq: this.inputSeq });
      }
    } finally {
      this.draining = false;
    }
  }

  private async sendWithRetry(params: T.TerminalInputParams): Promise<void> {
    for (let attempt = 0; ; attempt += 1) {
      try {
        await this.api.input(params);
        return;
      } catch (error) {
        if (this.disposed || isNotFound(error) || attempt >= this.maxRetries) {
          this.callbacks.onError?.(error);
          return;
        }
        await new Promise<void>((resolve) => setTimeout(resolve, this.retryMs * (attempt + 1)));
      }
    }
  }

  // --- resize ----------------------------------------------------------------------------

  /** The emulator changed size: the daemon hears about it 50 ms after the last change. */
  resize(cols: number, rows: number): void {
    this.resizeTo = { cols, rows };
    if (this.resizeTimer !== null) clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => {
      this.resizeTimer = null;
      const size = this.resizeTo;
      this.resizeTo = null;
      if (size) this.flushResize(size);
    }, this.resizeMs);
  }

  private flushResize(size: { cols: number; rows: number }): void {
    const id = this.id;
    if (id === null || this.disposed || this.exitCode !== undefined) return;
    this.api.resize({ id, cols: size.cols, rows: size.rows }).catch((error: unknown) => {
      this.callbacks.onError?.(error);
    });
  }

  // --- lifetime --------------------------------------------------------------------------

  dispose(): void {
    this.disposed = true;
    this.generation += 1;
    if (this.resizeTimer !== null) clearTimeout(this.resizeTimer);
    this.resizeTimer = null;
    this.dropHandle();
    this.queue = [];
  }
}
