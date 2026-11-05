// QA-only render counter and event ledger (M2-E4-T4, docs/07 section 10, ADR-0036).
//
// Call sites are guarded by `import.meta.env.MODE === "qa"` (a build-time constant), so Rollup
// drops both the calls and this module from production bundles; src/qa/sentinel.test.ts scans
// `dist` for RENDER_COUNTER_SENTINEL and `__FORGEBOX_RENDERS__`. Keep this module free of
// top-level side effects, or tree-shaking cannot remove it.
//
// What it answers for qa:perf (qa/perf/event-storm.spec.ts):
// - how many times each entity's component rendered, and the most in any one animation frame
//   (the "<= 1 commit per frame" budget);
// - which events reached the stores, by id (the Activity feed's input), so a reconnect can be
//   checked for duplicates.

/** Unique string that must never appear in a production bundle. */
export const RENDER_COUNTER_SENTINEL = "FORGEBOX_RENDER_COUNTER_7d2a41";

export interface RenderStat {
  total: number;
  /** The most renders of this entity inside a single animation frame. */
  maxPerFrame: number;
}

export interface LedgerEntry {
  id: string;
  seq: number;
}

export interface RenderCounterHandle {
  sentinel: string;
  /** Animation frames elapsed since the counter started. */
  frames: () => number;
  renders: () => Record<string, RenderStat>;
  ledger: () => LedgerEntry[];
  reset: () => void;
}

declare global {
  interface Window {
    __FORGEBOX_RENDERS__?: RenderCounterHandle;
  }
}

interface Slot {
  total: number;
  frame: number;
  inFrame: number;
  maxPerFrame: number;
}

interface Counters {
  slots: Map<string, Slot>;
  frame: () => number;
  push: (entries: LedgerEntry[]) => void;
}

let counters: Counters | null = null;

function install(win: Window): RenderCounterHandle {
  const slots = new Map<string, Slot>();
  let ledger: LedgerEntry[] = [];
  let frame = 0;
  const tick = () => {
    frame += 1;
    win.requestAnimationFrame(tick);
  };
  win.requestAnimationFrame(tick);
  const handle: RenderCounterHandle = {
    sentinel: RENDER_COUNTER_SENTINEL,
    frames: () => frame,
    renders: () =>
      Object.fromEntries(
        [...slots].map(([key, slot]) => [
          key,
          { total: slot.total, maxPerFrame: slot.maxPerFrame } satisfies RenderStat,
        ]),
      ),
    ledger: () => ledger.slice(),
    reset: () => {
      slots.clear();
      ledger = [];
    },
  };
  win.__FORGEBOX_RENDERS__ = handle;
  counters = { slots, frame: () => frame, push: (entries) => ledger.push(...entries) };
  return handle;
}

function active(): Counters | null {
  if (counters === null && typeof window !== "undefined") install(window);
  return counters;
}

/** Counts one render of `kind:id` (call from the component body). */
export function countRender(kind: string, id: string): void {
  const c = active();
  if (c === null) return;
  const key = `${kind}:${id}`;
  const now = c.frame();
  const slot = c.slots.get(key) ?? { total: 0, frame: -1, inFrame: 0, maxPerFrame: 0 };
  slot.total += 1;
  slot.inFrame = slot.frame === now ? slot.inFrame + 1 : 1;
  slot.frame = now;
  slot.maxPerFrame = Math.max(slot.maxPerFrame, slot.inFrame);
  c.slots.set(key, slot);
}

/** Records the events that reached the stores (after the stream's own de-duplication). */
export function recordEvents(events: readonly { id: string; seq: number }[]): void {
  active()?.push(events.map((e) => ({ id: e.id, seq: e.seq })));
}
