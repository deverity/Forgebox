// React binding for motion.ts: re-evaluates prefers-reduced-motion live (docs/07 section 3).
import { useSyncExternalStore } from "react";
import { REDUCED_MOTION_QUERY, prefersReducedMotion, resolveMotion } from "./motion";
import type { MotionPreset, PresetName } from "./motion";

function subscribe(onChange: () => void): () => void {
  if (typeof window.matchMedia !== "function") return () => undefined;
  const query = window.matchMedia(REDUCED_MOTION_QUERY);
  query.addEventListener("change", onChange);
  return () => {
    query.removeEventListener("change", onChange);
  };
}

export function useReducedMotion(): boolean {
  return useSyncExternalStore(subscribe, prefersReducedMotion, () => false);
}

/** Preset resolved for the current preference; `reduced` is true when it collapsed to the fade. */
export function useMotionPreset(name: PresetName): MotionPreset & { readonly reduced: boolean } {
  const reduced = useReducedMotion();
  return { ...resolveMotion(name, reduced), reduced };
}
