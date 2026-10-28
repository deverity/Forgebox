import { useState } from "react";
import { Switch } from "radix-ui";
import { motion } from "motion/react";
import { useMotionPreset } from "../useMotionPreset";
import { DISABLED, FOCUS_RING, cx, motionKind } from "./shared";

export interface ToggleProps {
  checked?: boolean;
  defaultChecked?: boolean;
  onCheckedChange?: (checked: boolean) => void;
  disabled?: boolean;
  /** Accessible name (a visible label, if any, lives next to the toggle in the caller). */
  label: string;
  id?: string;
}

const TRAVEL = 16; // px: track 36 - thumb 16 - padding 2 * 2

/**
 * Switch (Radix: role=switch, Space toggles). The thumb moves with the 500/40 spring; under
 * reduced motion it jumps and only the colour fades, over 100 ms (docs/07 section 3).
 */
export function Toggle({
  label,
  checked,
  defaultChecked = false,
  onCheckedChange,
  disabled = false,
  id,
}: ToggleProps) {
  const { transition, reduced } = useMotionPreset("toggle");
  const [inner, setInner] = useState(defaultChecked);
  const value = checked ?? inner;
  return (
    <Switch.Root
      {...(id !== undefined && { id })}
      checked={value}
      disabled={disabled}
      aria-label={label}
      data-motion={motionKind(reduced)}
      onCheckedChange={(next) => {
        setInner(next);
        onCheckedChange?.(next);
      }}
      className={cx(
        "group relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border border-stroke-strong bg-surface-2 p-0.5 transition-colors data-[state=checked]:border-transparent data-[state=checked]:bg-accent",
        FOCUS_RING,
        DISABLED,
      )}
      style={{ transitionDuration: reduced ? "var(--duration-reduced)" : "var(--duration-micro)" }}
    >
      <Switch.Thumb asChild>
        <motion.span
          className="block size-4 rounded-full bg-fg-secondary group-data-[state=checked]:bg-fg-on-accent"
          initial={false}
          animate={{ x: value ? TRAVEL : 0 }}
          transition={reduced ? { duration: 0 } : transition}
        />
      </Switch.Thumb>
    </Switch.Root>
  );
}
