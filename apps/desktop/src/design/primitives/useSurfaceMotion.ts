import type { TargetAndTransition, Transition } from "motion/react";
import { type PresetName, resolveMotion } from "../motion";

export interface SurfaceMotion {
  /** True when prefers-reduced-motion is on: opacity-only 100 ms fade (docs/07 section 3). */
  readonly reduced: boolean;
  readonly initial: TargetAndTransition;
  readonly animate: TargetAndTransition;
  readonly exit: TargetAndTransition;
  readonly transition: Transition;
  /** Spread on the animated element so tests and QA can read the resolved mode. */
  readonly dataMotion: "full" | "reduced";
}

type Kind = "sheet" | "popover" | "toast";

const PRESET: Record<Kind, PresetName> = { sheet: "sheet", popover: "popover", toast: "popover" };

const OFFSET: Record<Kind, TargetAndTransition> = {
  sheet: { x: 32 },
  popover: { scale: 0.96, y: -4 },
  toast: { y: 16, scale: 0.98 },
};

const REST: Record<Kind, TargetAndTransition> = {
  sheet: { x: 0 },
  popover: { scale: 1, y: 0 },
  toast: { y: 0, scale: 1 },
};

/**
 * Enter/exit variants for a floating surface. The preference is read at call time (each render),
 * so toggling prefers-reduced-motion applies without a reload. Only transform and opacity animate.
 */
export function surfaceMotion(kind: Kind): SurfaceMotion {
  const preset = resolveMotion(PRESET[kind]);
  if (preset.fadeOnly) {
    return {
      reduced: true,
      initial: { opacity: 0 },
      animate: { opacity: 1 },
      exit: { opacity: 0 },
      transition: preset.transition,
      dataMotion: "reduced",
    };
  }
  return {
    reduced: false,
    initial: { opacity: 0, ...OFFSET[kind] },
    animate: { opacity: 1, ...REST[kind] },
    exit: { opacity: 0, ...OFFSET[kind] },
    transition: preset.transition,
    dataMotion: "full",
  };
}
