import { useLayoutEffect, useRef, useState } from "react";
import { RadioGroup } from "radix-ui";
import { motion } from "motion/react";
import { useMotionPreset } from "../useMotionPreset";
import { FOCUS_RING, SEGMENT_PILL_TESTID, cx, motionKind } from "./shared";

export interface SegmentOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SegmentedControlProps {
  options: readonly SegmentOption[];
  value?: string;
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  disabled?: boolean;
  /** Accessible name of the group. */
  label: string;
}

interface PillBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

const SAME_BOX = (a: PillBox | null, b: PillBox | null) =>
  a === b ||
  (a !== null &&
    b !== null &&
    a.left === b.left &&
    a.top === b.top &&
    a.width === b.width &&
    a.height === b.height);

/**
 * Single-choice segmented control, exposed as a radiogroup (arrows move and select, Tab
 * leaves). The selection pill glides with the toggle spring (fade under reduced motion).
 *
 * The pill is positioned from the checked item's offsets inside the group (R-02): a shared
 * `layoutId` measured the page position, so any layout shift above the control (an ErrorNotice
 * entering) left the pill ~12 px off. Offsets relative to the group ignore what is around it.
 */
export function SegmentedControl({
  options,
  value,
  defaultValue,
  onValueChange,
  disabled = false,
  label,
}: SegmentedControlProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<PillBox | null>(null);
  const { transition, reduced } = useMotionPreset("toggle");
  const [inner, setInner] = useState(defaultValue ?? options[0]?.value ?? "");
  const current = value ?? inner;

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (root === null) return;
    const measure = () => {
      const item = root.querySelector<HTMLElement>('[role="radio"][data-state="checked"]');
      const next =
        item === null
          ? null
          : {
              left: item.offsetLeft,
              top: item.offsetTop,
              width: item.offsetWidth,
              height: item.offsetHeight,
            };
      setBox((prev) => (SAME_BOX(prev, next) ? prev : next));
    };
    measure();
    // Labels change with the language and fonts load late: re-measure on any size change.
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(root);
    root.querySelectorAll('[role="radio"]').forEach((item) => {
      observer.observe(item);
    });
    return () => {
      observer.disconnect();
    };
  }, [current, options]);

  return (
    <RadioGroup.Root
      ref={rootRef}
      value={current}
      disabled={disabled}
      aria-label={label}
      orientation="horizontal"
      data-motion={motionKind(reduced)}
      onValueChange={(next) => {
        setInner(next);
        onValueChange?.(next);
      }}
      className="relative inline-flex gap-0.5 rounded-control border border-stroke-strong bg-surface-2 p-0.5"
    >
      {box === null ? null : (
        <motion.span
          aria-hidden="true"
          data-testid={SEGMENT_PILL_TESTID}
          className="absolute rounded-control border border-stroke bg-surface"
          // Reduced motion: no slide; the pill fades in once and then just jumps.
          initial={reduced ? { ...box, opacity: 0 } : false}
          animate={reduced ? { ...box, opacity: 1 } : { ...box }}
          transition={reduced ? { duration: 0 } : transition}
        />
      )}
      {options.map((option) => {
        const isOn = option.value === current;
        return (
          <RadioGroup.Item
            key={option.value}
            value={option.value}
            disabled={option.disabled === true}
            className={cx(
              "relative h-6 rounded-control px-3 text-callout transition-colors duration-(--duration-micro) disabled:cursor-not-allowed disabled:opacity-50",
              isOn ? "text-fg-primary" : "text-fg-secondary hover:text-fg-primary",
              FOCUS_RING,
            )}
          >
            <span className="relative">{option.label}</span>
          </RadioGroup.Item>
        );
      })}
    </RadioGroup.Root>
  );
}
