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

/** Tracks marker positions across draws; hand `onDraw` to the canvas. */
export function useMarkerLayout(markers: readonly ChartMarker[]) {
  const [layout, setLayout] = useState<MarkerLayout>(EMPTY);
  const markersRef = useRef(markers);
  useLayoutEffect(() => {
    markersRef.current = markers;
  });
  const onDraw = useCallback((u: uPlot) => {
    const next = placeMarkers(u, markersRef.current);
    setLayout((prev) => (sameLayout(prev, next) ? prev : next));
  }, []);
  return { layout, onDraw };
}

const KIND_CLASS: Record<MarkerKind, string> = {
  "php-version": "bg-accent",
  restart: "bg-state-starting",
  config: "bg-state-updating",
  update: "bg-state-running",
};

/**
 * Event markers over the plot: a thin line with a focusable handle on top. Every handle is a
 * button (reachable by Tab) whose tooltip opens on hover and on focus. Transitions run only
 * under `motion-safe`.
 */
export function MarkersOverlay({ layout }: { layout: MarkerLayout }) {
  const { t } = useTranslation("metrics");
  const fmt = useFormatters();
  const items = useMemo(
    () =>
      layout.placements.map((p) => {
        const kind = t(`markers.${p.marker.kind}`);
        const when = fmt.date(p.marker.t * 1000, { dateStyle: "short", timeStyle: "medium" });
        const text = p.marker.detail ? `${kind}: ${p.marker.detail}` : kind;
        return { ...p, text, when };
      }),
    [layout.placements, t, fmt],
  );
  if (items.length === 0) return null;
  return (
    <div data-chart-markers="" className="pointer-events-none absolute inset-0">
      {items.map((m) => (
        <div
          key={m.marker.id}
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
