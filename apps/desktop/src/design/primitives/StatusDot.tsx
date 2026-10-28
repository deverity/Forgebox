import { motion } from "motion/react";
import type { TargetAndTransition, Transition } from "motion/react";
import { useTranslation } from "react-i18next";
import { motion as motionTokens } from "../tokens";
import { useReducedMotion } from "../useMotionPreset";
import { cx } from "./shared";

export type ServiceState = "running" | "starting" | "stopped" | "failed" | "updating";

export const SERVICE_STATES: readonly ServiceState[] = [
  "running",
  "starting",
  "stopped",
  "failed",
  "updating",
];

export type DotAnimation = "pulse" | "breathe" | "static" | "shake" | "fade";

const COLOR: Record<ServiceState, string> = {
  running: "bg-state-running",
  starting: "bg-state-starting",
  stopped: "bg-state-stopped",
  failed: "bg-state-failed",
  updating: "bg-state-updating",
};

const KIND: Record<ServiceState, DotAnimation> = {
  starting: "pulse",
  updating: "pulse",
  running: "breathe",
  stopped: "static",
  failed: "shake",
};

const LOOP: Transition = { repeat: Infinity, ease: "easeInOut" };

// docs/07 section 3 / docs/design/motion.md: pulse while starting, a 2 s breathe while running,
// static when stopped, a single shake on failure. Only transform and opacity animate.
const ANIMATE: Record<
  Exclude<DotAnimation, "fade">,
  { animate: TargetAndTransition; transition: Transition }
> = {
  pulse: {
    animate: { scale: [1, 1.35, 1], opacity: [1, 0.5, 1] },
    transition: { ...LOOP, duration: 1 },
  },
  breathe: { animate: { opacity: [1, 0.55, 1] }, transition: { ...LOOP, duration: 2 } },
  static: { animate: { opacity: 1 }, transition: { duration: 0 } },
  shake: {
    animate: { x: [0, -2, 2, -2, 2, 0] },
    transition: { duration: 0.3, ease: "easeOut" },
  },
};

export function animationFor(state: ServiceState, reduced: boolean): DotAnimation {
  return reduced ? "fade" : KIND[state];
}

/**
 * Service state indicator. The colour is never the only signal: the dot is an image with the
 * localized state as its accessible name (aria-label), and `showLabel` renders it as text too.
 */
export function StatusDot({
  state,
  showLabel = false,
}: {
  state: ServiceState;
  showLabel?: boolean;
}) {
  const { t } = useTranslation("common");
  const reduced = useReducedMotion();
  const kind = animationFor(state, reduced);
  const text = t(`status.${state}`);
  // Reduced motion: no pulse/scale/shake; a single 100 ms opacity fade when the state changes.
  const spec =
    kind === "fade"
      ? {
          initial: { opacity: 0.4 },
          animate: { opacity: 1 } satisfies TargetAndTransition,
          transition: {
            duration: motionTokens.duration.reduced / 1000,
            ease: "linear",
          } satisfies Transition,
        }
      : { initial: false as const, ...ANIMATE[kind] };
  return (
    <span className="inline-flex items-center gap-2">
      <motion.span
        key={state}
        role="img"
        aria-label={text}
        data-state={state}
        data-animation={kind}
        className={cx("inline-block size-2 rounded-full", COLOR[state])}
        {...spec}
      />
      {showLabel ? <span className="text-callout text-fg-secondary">{text}</span> : null}
    </span>
  );
}
