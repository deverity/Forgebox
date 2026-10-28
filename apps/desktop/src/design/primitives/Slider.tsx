import { Slider as RadixSlider } from "radix-ui";
import { FOCUS_RING, cx } from "./shared";

export interface SliderProps {
  value?: number;
  defaultValue?: number;
  onValueChange?: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  /** Accessible name of the thumb. */
  label: string;
  /** Optional localized text for the current value (aria-valuetext), e.g. "512 MB". */
  valueText?: string;
}

/** Single-thumb Radix slider: arrows +/- step, PageUp/PageDown x10, Home/End min/max. */
export function Slider({
  value,
  defaultValue,
  onValueChange,
  min = 0,
  max = 100,
  step = 1,
  disabled = false,
  label,
  valueText,
}: SliderProps) {
  return (
    <RadixSlider.Root
      {...(value !== undefined && { value: [value] })}
      {...(defaultValue !== undefined && { defaultValue: [defaultValue] })}
      {...(onValueChange && {
        onValueChange: (next: number[]) => {
          onValueChange(next[0] ?? min);
        },
      })}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      className={cx(
        "relative flex h-5 w-48 touch-none items-center select-none",
        disabled && "cursor-not-allowed opacity-50",
      )}
    >
      <RadixSlider.Track className="relative h-1 grow rounded-full bg-stroke-strong">
        <RadixSlider.Range className="absolute h-full rounded-full bg-accent" />
      </RadixSlider.Track>
      <RadixSlider.Thumb
        aria-label={label}
        {...(valueText !== undefined && { "aria-valuetext": valueText })}
        className={cx(
          "block size-4 rounded-full border border-stroke-strong bg-surface shadow-popover",
          FOCUS_RING,
        )}
      />
    </RadixSlider.Root>
  );
}
