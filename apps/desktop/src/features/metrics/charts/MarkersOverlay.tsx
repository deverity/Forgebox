import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type uPlot from "uplot";
import { Tooltip } from "../../../design/primitives/Tooltip";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { FOCUS_RING } from "../../../design/primitives/shared";
import type { ChartMarker, MarkerKind } from "./types";

export interface MarkerPlacement {
  marker: ChartMarker;
  /** CSS px from the left edge of the chart host. */
  left: number;
}

export interface MarkerLayout {
  top: number;
  height: number;
  placements: MarkerPlacement[];
}

const EMPTY: MarkerLayout = { top: 0, height: 0, placements: [] };

/** Same markers (by identity) over the same plot box; only the x positions may differ. */
function sameStructure(a: MarkerLayout, b: MarkerLayout): boolean {
  return (
    a.top === b.top &&
    a.height === b.height &&
    a.placements.length === b.placements.length &&
    a.placements.every((p, i) => b.placements.at(i)?.marker === p.marker)
  );
}

function sameLayout(a: MarkerLayout, b: MarkerLayout): boolean {
  return (
    a.top === b.top &&
    a.height === b.height &&
    a.placements.length === b.placements.length &&
    a.placements.every((p, i) => {
      const q = b.placements.at(i);
      return q?.marker === p.marker && q.left === p.left;
    })
  );
}

/** Pure placement: markers inside the visible x range, positioned over the plot area. */
export function placeMarkers(u: uPlot, markers: readonly ChartMarker[]): MarkerLayout {
  const x: Partial<uPlot.Scale> | undefined = u.scales.x;
  const min = x?.min;
  const max = x?.max;
  if (min === undefined || max === undefined) return EMPTY;
  const origin = u.over.offsetLeft;
  const placements = markers
    .filter((m) => m.t >= min && m.t <= max)
    .map((marker) => ({ marker, left: Math.round(origin + u.valToPos(marker.t, "x")) }));
  return { top: u.over.offsetTop, height: u.over.offsetHeight, placements };
}

/** Moves the rendered handles to `next`; false when the DOM does not hold exactly those markers. */
function writeLefts(root: HTMLElement | null, next: MarkerLayout): boolean {
  if (!root) return false;
  if (root.children.length !== next.placements.length) return false;
  const slots: HTMLElement[] = [];
  for (const [i, p] of next.placements.entries()) {
    const slot = root.children.item(i);
    if (!(slot instanceof HTMLElement) || slot.dataset.markerId !== p.marker.id) return false;
    slots.push(slot);
  }
  for (const [i, slot] of slots.entries()) {
    slot.style.left = `${String(next.placements[i]?.left ?? 0)}px`;
  }
  return true;
}

type SettledListener = () => void;
const settledListeners = new Set<SettledListener>();

/**
 * Subscribes to "the marker layout of a draw is committed" (QA harness: the update budget counts
 * the overlay's commit, not only the canvas). Fires once per draw, after the DOM is up to date.
 */
export function onMarkerLayoutSettled(listener: SettledListener): () => void {
  settledListeners.add(listener);
  return () => {
    settledListeners.delete(listener);
  };
}

function emitSettled(): void {
  for (const listener of [...settledListeners]) listener();
}

/**
 * Tracks marker positions; hand `onDraw` to the canvas. The layout is recomputed on every draw
 * (the x range moved) and also when `markers` change without a draw (markers arriving from a
 * separate query, a refetch with new objects), using the last drawn uPlot instance.
 */
export function useMarkerLayout(markers: readonly ChartMarker[]) {
  const [layout, setLayout] = useState<MarkerLayout>(EMPTY);
  const layoutRef = useRef(EMPTY);
  const markersRef = useRef(markers);
  const lastPlot = useRef<uPlot | null>(null);
  const settleOnCommit = useRef(false);

  const overlayRef = useRef<HTMLDivElement>(null);

  const apply = useCallback((next: MarkerLayout, fromDraw: boolean) => {
    if (sameLayout(layoutRef.current, next)) {
      if (fromDraw) emitSettled();
      return;
    }
    // Hot path (every live update moves the x range): the same markers only shift sideways, so
    // their `left` is written straight to the DOM and React does not re-render the overlay.
    if (sameStructure(layoutRef.current, next) && writeLefts(overlayRef.current, next)) {
      layoutRef.current = next;
      if (fromDraw) emitSettled();
      return;
    }
    layoutRef.current = next;
    settleOnCommit.current = fromDraw;
    setLayout(next);
  }, []);

  useLayoutEffect(() => {
    markersRef.current = markers;
    const u = lastPlot.current;
    if (u) apply(placeMarkers(u, markers), false);
  }, [markers, apply]);

  useLayoutEffect(() => {
    // React's `left` may be older than a direct write (see `apply`): the DOM follows the ref.
    writeLefts(overlayRef.current, layoutRef.current);
    if (settleOnCommit.current) {
      settleOnCommit.current = false;
      emitSettled();
    }
  }, [layout]);

  const onDraw = useCallback(
    (u: uPlot) => {
      lastPlot.current = u;
      apply(placeMarkers(u, markersRef.current), true);
    },
    [apply],
  );
  return { layout, onDraw, overlayRef };
}

const KIND_CLASS: Record<MarkerKind, string> = {
  // Its own token: the accent is already the p50/CPU/rps line colour.
  "php-version": "bg-tool-4",
  restart: "bg-state-starting",
  config: "bg-state-updating",
  update: "bg-state-running",
};

/**
 * Event markers over the plot: a thin line with a focusable handle on top. Every handle is a
 * button (reachable by Tab) whose tooltip opens on hover and on focus. Transitions run only
 * under `motion-safe`.
 */
export function MarkersOverlay({
  layout,
  overlayRef,
}: {
  layout: MarkerLayout;
  overlayRef?: React.Ref<HTMLDivElement>;
}) {
  const { t } = useTranslation("metrics");
  const fmt = useFormatters();
  const items = useMemo(
    () =>
      layout.placements.map((p) => {
        const kind = t(`charts.markers.${p.marker.kind}`);
        const when = fmt.date(p.marker.t * 1000, { dateStyle: "short", timeStyle: "medium" });
        const text = p.marker.detail ? `${kind}: ${p.marker.detail}` : kind;
        return { ...p, text, when };
      }),
    [layout.placements, t, fmt],
  );
  if (items.length === 0) return null;
  return (
    <div ref={overlayRef} data-chart-markers="" className="pointer-events-none absolute inset-0">
      {items.map((m) => (
        <div
          key={m.marker.id}
          data-marker-id={m.marker.id}
          style={{ left: m.left, top: layout.top, height: layout.height }}
          className="absolute w-0"
        >
          <span
            aria-hidden="true"
            className={`absolute top-0 bottom-0 -left-px w-px opacity-60 ${KIND_CLASS[m.marker.kind]}`}
          />
          <Tooltip
            content={
              <>
                <div>{m.text}</div>
                <div className="text-caption text-fg-secondary">{m.when}</div>
              </>
            }
            delayMs={150}
          >
            <button
              type="button"
              data-marker-kind={m.marker.kind}
              aria-label={`${m.text}, ${m.when}`}
              className={`pointer-events-auto absolute -top-1 -left-1.5 size-3 cursor-default rounded-full border border-surface ${FOCUS_RING} motion-safe:transition-transform motion-safe:hover:scale-125 ${KIND_CLASS[m.marker.kind]}`}
            />
          </Tooltip>
        </div>
      ))}
    </div>
  );
}
