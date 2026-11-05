import { useEffect, useLayoutEffect, useRef, useState } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { readPalette, type ChartPalette } from "./palette";
import { useThemeKey } from "./useChartTheme";

export type BuildOptions = (
  palette: ChartPalette,
  size: { width: number; height: number },
) => Omit<uPlot.Options, "width" | "height">;

export interface UPlotCanvasProps {
  build: BuildOptions;
  data: uPlot.AlignedData;
  height: number;
  /** Fixed width (sparklines); omitted = follows the container. */
  width?: number;
  /** Called after every draw, with the live chart (marker positions, tests). */
  onDraw?: (u: uPlot) => void;
}

/**
 * One uPlot instance bound to React. Data changes call `setData` (no re-creation); the chart is
 * re-created only when the palette (theme key) or the options change. Device pixel ratio changes
 * need no work here: uPlot listens to a resolution media query itself and re-renders its canvases
 * at the new ratio (`dppxchange`). CDP's deviceScaleFactor emulation does not fire that query, so
 * the QA matrix opens a fresh page per DPR instead of switching it live.
 * Width follows the container through a ResizeObserver (`setSize`). The canvas is
 * decorative (`aria-hidden`): the figure carries the accessible summary table.
 */
export function UPlotCanvas({ build, data, height, width, onDraw }: UPlotCanvasProps) {
  const host = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const latest = useRef({ data, onDraw });
  const lastData = useRef<uPlot.AlignedData | null>(null);
  const widthRef = useRef(width ?? 0);
  const [measured, setMeasured] = useState(width ?? 0);
  const themeKey = useThemeKey();
  const currentWidth = width ?? measured;

  // Keep the refs the effects read current (layout effects run before the creating effect).
  useLayoutEffect(() => {
    latest.current = { data, onDraw };
    widthRef.current = currentWidth;
  });

  // Follow the container width.
  useEffect(() => {
    const el = host.current;
    if (!el || width !== undefined) return;
    const apply = () => {
      setMeasured(Math.max(0, Math.floor(el.clientWidth)));
    };
    apply();
    const observer = new ResizeObserver(apply);
    observer.observe(el);
    return () => {
      observer.disconnect();
    };
  }, [width]);

  const ready = currentWidth > 0;

  // (Re)create on palette / DPR / options change.
  useEffect(() => {
    const el = host.current;
    if (!el || !ready) return;
    const palette = readPalette(el);
    const options = build(palette, { width: widthRef.current, height });
    const instance = new uPlot(
      {
        ...options,
        width: widthRef.current,
        height,
        hooks: {
          ...options.hooks,
          draw: [...(options.hooks?.draw ?? []), (u) => latest.current.onDraw?.(u)],
        },
      },
      latest.current.data,
      el,
    );
    plot.current = instance;
    lastData.current = latest.current.data;
    return () => {
      instance.destroy();
      plot.current = null;
    };
  }, [build, height, themeKey, ready]);

  // Data updates: in place, in the commit phase so the draw follows the state change directly.
  useLayoutEffect(() => {
    const instance = plot.current;
    if (!instance || lastData.current === data) return;
    lastData.current = data;
    instance.setData(data);
  }, [data]);

  // Width changes: resize without re-creating.
  useEffect(() => {
    const instance = plot.current;
    if (instance && ready && (instance.width !== currentWidth || instance.height !== height)) {
      instance.setSize({ width: currentWidth, height });
    }
  }, [currentWidth, height, ready]);

  return (
    <div
      ref={host}
      aria-hidden="true"
      data-chart-canvas=""
      style={{ height, width: width ?? "100%" }}
      className="relative"
    />
  );
}
