import { afterEach, describe, expect, it, vi } from "vitest";
import { FrameBatcher, HIDDEN_FLUSH_MS, manualFrameClock } from "./frame";
import { manualPageActivity } from "./page";

afterEach(() => {
  vi.useRealTimers();
});

function setup(options: { maxBatch?: number; maxQueue?: number; visible?: boolean } = {}) {
  const frame = manualFrameClock();
  const page = manualPageActivity(options.visible ?? true);
  const batches: number[][] = [];
  const overflow = vi.fn();
  const batcher = new FrameBatcher<number>({
    apply: (batch) => batches.push([...batch]),
    frame,
    page,
    onOverflow: overflow,
    ...(options.maxBatch === undefined ? {} : { maxBatch: options.maxBatch }),
    ...(options.maxQueue === undefined ? {} : { maxQueue: options.maxQueue }),
  });
  return { frame, page, batches, overflow, batcher };
}

describe("FrameBatcher", () => {
  it("applies everything pushed before a frame in one batch", () => {
    const { frame, batches, batcher } = setup();
    batcher.push([1, 2]);
    batcher.push([3]);
    expect(batches).toEqual([]);
    expect(frame.pending()).toBe(1);
    frame.tick();
    expect(batches).toEqual([[1, 2, 3]]);
    expect(frame.pending()).toBe(0);
  });

  it("caps a batch and carries the rest to the next frames", () => {
    const { frame, batches, batcher } = setup({ maxBatch: 2 });
    batcher.push([1, 2, 3, 4, 5]);
    frame.tick();
    frame.tick();
    frame.tick();
    expect(batches).toEqual([[1, 2], [3, 4], [5]]);
  });

  it("flushes on every visibility change", () => {
    const { page, batches, batcher } = setup();
    batcher.push([1]);
    page.set({ visible: false });
    expect(batches).toEqual([[1]]);
  });

  it("uses a timer while hidden (no rAF), so the queue never grows unbounded", () => {
    vi.useFakeTimers();
    const { frame, batches, batcher } = setup({ visible: false });
    batcher.push([1, 2]);
    expect(frame.pending()).toBe(0);
    vi.advanceTimersByTime(HIDDEN_FLUSH_MS);
    expect(batches).toEqual([[1, 2]]);
  });

  it("drops the queue and reports an overflow past maxQueue", () => {
    const { frame, batches, batcher, overflow } = setup({ maxQueue: 3 });
    batcher.push([1, 2]);
    batcher.push([3, 4]);
    expect(overflow).toHaveBeenCalledOnce();
    expect(batcher.size).toBe(0);
    frame.tick();
    expect(batches).toEqual([]);
  });

  it("stops after dispose", () => {
    const { frame, batches, batcher, page } = setup();
    batcher.push([1]);
    batcher.dispose();
    frame.tick();
    page.set({ visible: false });
    batcher.push([2]);
    expect(batches).toEqual([]);
  });
});
