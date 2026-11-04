import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { DEFAULT_CHART_HEIGHT } from "./ChartFrame";
import {
  ChartSparkline,
  LatencyBandChart,
  ResourceChart,
  ThroughputErrorsChart,
  type ChartMarker,
  type LatencySeries,
  type ResourceSeries,
  type ThroughputSeries,
} from "./index";

// Dev/qa harness of the charts (route `/__charts`, never in production builds). Deterministic
// data (fixed clock and seed) so screenshots are stable. Query params:
//   state=data|empty|loading   points=<n>   only=latency|throughput|resource   markers=off
// `window.__charts.run(n)` appends one point per update (n updates) to the visible chart and
// resolves with the milliseconds each update took, from state change to the canvas being drawn
// (qa/checks/charts.spec.ts, acceptance criterion 2).

/** 2026-03-14T12:00:00Z, the fixed clock of the QA stills. */
const END = 1_773_489_600;
const STEP_S = 2;

interface Frame {
  t: number[];
  p50: number[];
  p90: number[];
  p99: number[];
  php: number[];
  rps: number[];
  e4xx: number[];
  e5xx: number[];
  cpu: number[];
  ram: number[];
}

/** Small deterministic generator (mulberry32). */
function rng(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function point(i: number, rand: () => number) {
  const wave = Math.sin(i / 40);
  const p50 = 30 + 10 * wave + rand() * 6;
  const p90 = p50 * (1.8 + rand() * 0.4);
  const p99 = p90 * (1.6 + rand() * 0.6);
  const rps = Math.max(0, 40 + 25 * wave + rand() * 10);
  return {
    p50,
    p90,
    p99,
    php: p50 * 0.7,
    rps,
    e4xx: rps * 0.04 * rand(),
    e5xx: rps * 0.02 * rand() * (i % 97 < 6 ? 6 : 1),
    cpu: 20 + 15 * wave + rand() * 8,
    ram: (180 + 40 * wave + rand() * 6) * 1024 * 1024,
  };
}

function makeFrame(count: number): Frame {
  const rand = rng(7);
  const f: Frame = {
    t: [],
    p50: [],
    p90: [],
    p99: [],
    php: [],
    rps: [],
    e4xx: [],
    e5xx: [],
    cpu: [],
    ram: [],
  };
  for (let i = 0; i < count; i += 1) {
    append(f, END - (count - 1 - i) * STEP_S, i, rand);
  }
  return f;
}

function append(f: Frame, t: number, i: number, rand: () => number): void {
  const p = point(i, rand);
  f.t.push(t);
  for (const key of ["p50", "p90", "p99", "php", "rps", "e4xx", "e5xx", "cpu", "ram"] as const) {
    f[key].push(p[key]);
  }
}

/** Next frame: one point appended, the oldest dropped (a fixed-size window). */
function slide(f: Frame, i: number, rand: () => number): Frame {
  const next: Frame = {
    t: f.t.slice(1),
    p50: f.p50.slice(1),
    p90: f.p90.slice(1),
    p99: f.p99.slice(1),
    php: f.php.slice(1),
    rps: f.rps.slice(1),
    e4xx: f.e4xx.slice(1),
    e5xx: f.e5xx.slice(1),
    cpu: f.cpu.slice(1),
    ram: f.ram.slice(1),
  };
  append(next, (f.t.at(-1) ?? END) + STEP_S, i, rand);
  return next;
}

function latency(f: Frame): LatencySeries {
  return { t: f.t, p50: f.p50, p90: f.p90, p99: f.p99, php: f.php };
}
function throughput(f: Frame): ThroughputSeries {
  return { t: f.t, rps: f.rps, e4xx: f.e4xx, e5xx: f.e5xx };
}
function resource(f: Frame): ResourceSeries {
  return { t: f.t, cpu: f.cpu, ram: f.ram };
}

type Only = "latency" | "throughput" | "resource";

declare global {
  interface Window {
    __charts?: { run: (updates: number) => Promise<number[]> };
  }
}

const HEIGHT = DEFAULT_CHART_HEIGHT;

function Slot({ children }: { children: React.ReactNode }) {
  return <Suspense fallback={<Skeleton height={HEIGHT} />}>{children}</Suspense>;
}

export default function ChartsHarness() {
  const params = new URLSearchParams(window.location.search);
  const state = params.get("state") ?? "data";
  const count = Number(params.get("points") ?? "120");
  const only = params.get("only") as Only | null;
  const withMarkers = params.get("markers") !== "off";

  const [frame, setFrame] = useState<Frame>(() => makeFrame(count));
  const frameRef = useRef(frame);
  const counter = useRef(count);
  const rand = useRef(rng(99));

  useEffect(() => {
    window.__charts = {
      run: async (updates) => {
        const times: number[] = [];
        for (let n = 0; n < updates; n += 1) {
          // The window slide stands in for the data layer (M2-E5-T14) and is not timed.
          const next = slide(frameRef.current, counter.current, rand.current);
          const started = performance.now();
          counter.current += 1;
          frameRef.current = next;
          flushSync(() => {
            setFrame(next);
          });
          // uPlot commits (draws) in a microtask after setData.
          await Promise.resolve();
          times.push(performance.now() - started);
          // Let the browser breathe like the real 2 s cadence would.
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }
        return times;
      },
    };
    return () => {
      delete window.__charts;
    };
  }, []);

  const markers = useMemo<ChartMarker[]>(() => {
    const at = (back: number) => END - back * STEP_S;
    return [
      { id: "m1", t: at(Math.floor(count * 0.7)), kind: "php-version", detail: "8.3 → 8.4" },
      { id: "m2", t: at(Math.floor(count * 0.45)), kind: "restart" },
      { id: "m3", t: at(Math.floor(count * 0.25)), kind: "config", detail: "opcache" },
      { id: "m4", t: at(Math.floor(count * 0.1)), kind: "update" },
    ];
  }, [count]);

  const loading = state === "loading";
  const empty = state === "empty";
  const lat = empty || loading ? null : latency(frame);
  const thr = empty || loading ? null : throughput(frame);
  const res = empty || loading ? null : resource(frame);
  const show = (name: Only) => only === null || only === name;

  return (
    <main
      className="mx-auto flex max-w-4xl flex-col gap-6 bg-canvas p-6 text-fg-primary"
      data-charts-harness=""
    >
      {show("latency") ? (
        <Slot>
          <LatencyBandChart series={lat} markers={withMarkers ? markers : []} loading={loading} />
        </Slot>
      ) : null}
      {show("throughput") ? (
        <Slot>
          <ThroughputErrorsChart
            series={thr}
            markers={withMarkers ? markers : []}
            loading={loading}
          />
        </Slot>
      ) : null}
      {show("resource") ? (
        <Slot>
          <ResourceChart series={res} markers={withMarkers ? markers : []} loading={loading} />
        </Slot>
      ) : null}
      {only === null && !loading && !empty ? (
        <Slot>
          <ChartSparkline t={frame.t.slice(-60)} values={frame.rps.slice(-60)} />
        </Slot>
      ) : null}
    </main>
  );
}
