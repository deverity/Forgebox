import { useId, useState } from "react";
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

/**
 * Single-choice segmented control, exposed as a radiogroup (arrows move and select, Tab
 * leaves). The selection pill glides with the toggle spring (fade under reduced motion).
 */
export function SegmentedControl({
  options,
  value,
  defaultValue,
  onValueChange,
  disabled = false,
  label,
}: SegmentedControlProps) {
  const pillId = useId();
  const { transition, reduced } = useMotionPreset("toggle");
  const [inner, setInner] = useState(defaultValue ?? options[0]?.value ?? "");
  const current = value ?? inner;
  return (
    <RadioGroup.Root
      value={current}
      disabled={disabled}
      aria-label={label}
      orientation="horizontal"
      data-motion={motionKind(reduced)}
      onValueChange={(next) => {
        setInner(next);
        onValueChange?.(next);
      }}
      className="inline-flex gap-0.5 rounded-control border border-stroke-strong bg-surface-2 p-0.5"
    >
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
            {isOn ? (
              <motion.span
                layoutId={pillId}
                aria-hidden="true"
                data-testid={SEGMENT_PILL_TESTID}
                className="absolute inset-0 rounded-control border border-stroke bg-surface"
                transition={reduced ? { duration: 0.1, ease: "linear" } : transition}
              />
            ) : null}
            <span className="relative">{option.label}</span>
          </RadioGroup.Item>
        );
      })}
    </RadioGroup.Root>
  );
}
