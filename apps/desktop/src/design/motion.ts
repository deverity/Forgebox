// Motion presets (docs/07 section 3, docs/design/motion.md). Values come from tokens.ts;
// the shapes match Motion's `transition` prop so they can be spread straight into components.
import { motion } from "./tokens";

export type PresetName = "layout" | "popover" | "toggle" | "micro" | "transition" | "sheet";

export type Transition =
  | { readonly type: "spring"; readonly stiffness: number; readonly damping: number }
  | { readonly type: "tween"; readonly duration: number; readonly ease: "easeOut" | "linear" };

export interface MotionPreset {
  readonly transition: Transition;
  /** True when movement/scale must be dropped and only opacity may animate. */
  readonly fadeOnly: boolean;
}

export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

const seconds = (ms: number) => ms / 1000;

const spring = (name: keyof typeof motion.spring): Transition => ({
  type: "spring",
  ...motion.spring[name],
});

const tween = (name: keyof typeof motion.duration): Transition => ({
  type: "tween",
  duration: seconds(motion.duration[name]),
  ease: "easeOut",
});

export const presets: Readonly<Record<PresetName, MotionPreset>> = {
  layout: { transition: spring("layout"), fadeOnly: false },
  popover: { transition: spring("popover"), fadeOnly: false },
  toggle: { transition: spring("toggle"), fadeOnly: false },
  micro: { transition: tween("micro"), fadeOnly: false },
  transition: { transition: tween("transition"), fadeOnly: false },
  sheet: { transition: tween("sheet"), fadeOnly: false },
};

/** The single preset every interaction collapses to under prefers-reduced-motion. */
export const reducedPreset: MotionPreset = {
  transition: { type: "tween", duration: seconds(motion.duration.reduced), ease: "linear" },
  fadeOnly: true,
};

export function prefersReducedMotion(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

/** Resolves a preset, honouring prefers-reduced-motion (read at call time, not import time). */
export function resolveMotion(
  name: PresetName,
  reduced: boolean = prefersReducedMotion(),
): MotionPreset {
  return reduced ? reducedPreset : presets[name];
}
