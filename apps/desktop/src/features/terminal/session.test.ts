import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamEntry, StreamHandle } from "../../ipc/bridge";
import type * as T from "../../ipc/generated";
import { TerminalSession, type SessionApi, type TermPort } from "./session";

const DTO: T.TerminalDto = {
  id: "01TERM",
  title: "Terminal",
  shell: "pwsh",
  cwd: "C:/Forgebox",
  cols: 100,
  rows: 40,
  pid: 1,
  started_at: "2026-11-03T10:00:00Z",
  last_seq: 0,
};

type Entries = StreamEntry<T.TerminalEnvelope>[];

function output(seq: number, data: string): StreamEntry<T.TerminalEnvelope> {
  return { kind: "item", item: { subscription_id: "S", seq, item: { kind: "output", data } } };
}

function exited(seq: number, code: number | null): StreamEntry<T.TerminalEnvelope> {
  return {
    kind: "item",
    item: { subscription_id: "S", seq, item: { kind: "exited", exit_code: code } },
  };
}

class FakeTerm implements TermPort {
  cols = 100;
  rows = 40;
  written: string[] = [];
  resets = 0;
  private callbacks: (() => void)[] = [];
  write(data: string, callback?: () => void): void {
    this.written.push(data);
    if (callback) this.callbacks.push(callback);
  }
  reset(): void {
    this.resets += 1;
  }
  /** The emulator finished parsing the oldest pending write. */
  ack(): void {
    this.callbacks.shift()?.();
  }
  get pending(): number {
    return this.callbacks.length;
  }
}

interface Harness {
  term: FakeTerm;
  session: TerminalSession;
  api: {
    open: ReturnType<typeof vi.fn>;
    input: ReturnType<typeof vi.fn>;
    resize: ReturnType<typeof vi.fn>;
    subscribe: ReturnType<typeof vi.fn>;
  };
  streams: {
    params: T.TerminalSubscribeParams;
    emit: (entries: Entries) => void;
    handle: StreamHandle;
    unsub: ReturnType<typeof vi.fn>;
  }[];
  exits: (number | null)[];
  fit: ReturnType<typeof vi.fn>;
}

function harness(
  overrides: Partial<{ input: SessionApi["input"]; chunkChars: number }> = {},
): Harness {
  const term = new FakeTerm();
  const streams: Harness["streams"] = [];
  const exits: (number | null)[] = [];
  const api = {
    open: vi.fn((params: T.TerminalOpenParams) =>
      Promise.resolve({ ...DTO, cols: params.cols ?? 0, rows: params.rows ?? 0 }),
    ),
    input: vi.fn(overrides.input ?? (() => Promise.resolve({ ok: true }))),
    resize: vi.fn(() => Promise.resolve({ ok: true })),
    subscribe: vi.fn((params: T.TerminalSubscribeParams, onEntries: (entries: Entries) => void) => {
      const unsub = vi.fn(() => Promise.resolve());
      const handle: StreamHandle = {
        id: `h${String(streams.length)}`,
        method: "terminal.subscribe",
        resumePoint: null,
        pending: false,
        unsubscribe: unsub,
      };
      streams.push({ params, emit: onEntries, handle, unsub });
      return Promise.resolve(handle);
    }),
  };
  const fit = vi.fn(() => ({ cols: 132, rows: 43 }));
  const session = new TerminalSession({
    writer: "01WRITER",
    term,
    api,
    fit,
    callbacks: {
      onExit: (code) => {
        exits.push(code);
      },
    },
    retryDelayMs: 5,
    ...(overrides.chunkChars === undefined ? {} : { chunkChars: overrides.chunkChars }),
  });
  return { term, session, api, streams, exits, fit };
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("open", () => {
  it("opens with the cols/rows the fit addon measured and streams the whole ring", async () => {
    const h = harness();
    await h.session.open({ site: "loja" });
    expect(h.fit).toHaveBeenCalledOnce();
    expect(h.api.open).toHaveBeenCalledWith({ site: "loja", cols: 132, rows: 43 });
    expect(h.streams).toHaveLength(1);
    expect(h.streams[0]?.params).toEqual({ id: "01TERM", resume_from: null });
  });

  it("writes output one chunk at a time, after each write callback", async () => {
    const h = harness();
    await h.session.open();
    const stream = h.streams[0];
    stream?.emit([output(3, "abc"), output(6, "def"), output(9, "ghi")]);
    expect(h.term.written).toEqual(["abc"]); // the first write is in flight
    h.term.ack();
    expect(h.term.written).toEqual(["abc", "defghi"]); // what queued meanwhile is joined
    expect(h.session.lastSeq).toBe(9);
  });

  it("splits an output bigger than the chunk size", async () => {
    const h = harness({ chunkChars: 4 });
    await h.session.open();
    h.streams[0]?.emit([output(10, "0123456789")]);
    expect(h.term.written).toEqual(["0123"]);
    h.term.ack();
    h.term.ack();
    expect(h.term.written).toEqual(["0123", "4567", "89"]);
  });
});

describe("input", () => {
  it("sends strictly increasing input_seq with one request in flight", async () => {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let first = true;
    const h = harness({
      input: () => {
        if (first) {
          first = false;
          return gate.then(() => ({ ok: true }));
        }
        return Promise.resolve({ ok: true });
      },
    });
    await h.session.open();
    h.session.sendInput("a");
    h.session.sendInput("b");
    h.session.sendInput("c");
    expect(h.api.input).toHaveBeenCalledTimes(1); // "b" and "c" wait for the first answer
    release();
    await vi.advanceTimersByTimeAsync(0);
    const seqs = h.api.input.mock.calls.map((call) => (call[0] as T.TerminalInputParams).input_seq);
    const data = h.api.input.mock.calls.map((call) => (call[0] as T.TerminalInputParams).data);
    expect(seqs).toEqual([1, 2]);
    expect(data).toEqual(["a", "bc"]);
    h.session.sendInput("d");
    await vi.advanceTimersByTimeAsync(0);
    const all = h.api.input.mock.calls.map((call) => (call[0] as T.TerminalInputParams).input_seq);
    expect(all).toEqual([1, 2, 3]);
    expect((h.api.input.mock.calls[0]?.[0] as T.TerminalInputParams).writer).toBe("01WRITER");
  });

  it("retries a failed input with the same input_seq", async () => {
    let calls = 0;
    const h = harness({
      input: () => {
        calls += 1;
        return calls < 3 ? Promise.reject(new Error("net")) : Promise.resolve({ ok: true });
      },
    });
    await h.session.open();
    h.session.sendInput("x");
    await vi.advanceTimersByTimeAsync(100);
    const seqs = h.api.input.mock.calls.map((call) => (call[0] as T.TerminalInputParams).input_seq);
    expect(seqs).toEqual([1, 1, 1]);
  });

  it("keeps what was typed before the session opened", async () => {
    const h = harness();
    h.session.sendInput("ls");
    expect(h.api.input).not.toHaveBeenCalled();
    await h.session.open();
    await vi.advanceTimersByTimeAsync(0);
    expect((h.api.input.mock.calls[0]?.[0] as T.TerminalInputParams).data).toBe("ls");
  });
});

describe("resize", () => {
  it("debounces to 50 ms and sends only the last size", async () => {
    const h = harness();
    await h.session.open();
    h.session.resize(100, 30);
    await vi.advanceTimersByTimeAsync(30);
    h.session.resize(110, 31);
    await vi.advanceTimersByTimeAsync(49);
    expect(h.api.resize).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(h.api.resize).toHaveBeenCalledOnce();
    expect(h.api.resize).toHaveBeenCalledWith({ id: "01TERM", cols: 110, rows: 31 });
  });
});

describe("exit", () => {
  it("reports the exit code after the pending output was written", async () => {
    const h = harness();
    await h.session.open();
    h.streams[0]?.emit([output(2, "bye"), exited(3, 7)]);
    expect(h.exits).toEqual([]); // "bye" is still being parsed
    h.term.ack();
    expect(h.exits).toEqual([7]);
  });

  it("reports a missing exit code as null", async () => {
    const h = harness();
    await h.session.open();
    h.streams[0]?.emit([exited(1, null)]);
    expect(h.exits).toEqual([null]);
  });
});

describe("lagged", () => {
  it("resets the emulator and replays the whole ring", async () => {
    const h = harness();
    await h.session.open();
    h.streams[0]?.emit([output(5, "stale")]);
    h.streams[0]?.emit([{ kind: "lagged", missed: 12 }, output(99, "ignored")]);
    expect(h.term.resets).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.streams).toHaveLength(2);
    expect(h.streams[1]?.params).toEqual({ id: "01TERM", resume_from: null });
    expect(h.streams[0]?.unsub).toHaveBeenCalled();
    // The old stream is dead: late entries of it are ignored.
    h.streams[0]?.emit([output(120, "late")]);
    expect(h.term.written).toEqual(["stale"]);
    h.streams[1]?.emit([output(4, "new!")]);
    h.term.ack(); // the discarded write must not advance the new queue by itself
    expect(h.term.written.at(-1)).toBe("new!");
  });

  it("does the same on reset (the daemon restarted)", async () => {
    const h = harness();
    await h.session.open();
    h.streams[0]?.emit([{ kind: "reset" }]);
    expect(h.term.resets).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.streams[1]?.params.resume_from).toBeNull();
  });
});

describe("attach", () => {
  it("resumes from the listed last_seq and nudges the pseudo-console with a resize", async () => {
    const h = harness();
    await h.session.attach("01OLD", 4096);
    expect(h.streams[0]?.params).toEqual({ id: "01OLD", resume_from: 4096 });
    expect(h.api.resize).toHaveBeenCalledWith({ id: "01OLD", cols: 132, rows: 43 });
    expect(h.session.lastSeq).toBe(4096);
  });
});

describe("dispose", () => {
  it("unsubscribes and ignores later entries", async () => {
    const h = harness();
    await h.session.open();
    h.session.dispose();
    expect(h.streams[0]?.unsub).toHaveBeenCalled();
    h.streams[0]?.emit([output(1, "x")]);
    expect(h.term.written).toEqual([]);
  });
});
