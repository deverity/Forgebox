import { Select as RadixSelect } from "radix-ui";
import { DISABLED, FIELD_BASE, FIELD_BORDER, FIELD_WIDTH, FOCUS_RING, cx } from "./shared";
import type { FieldWidth } from "./shared";

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectProps {
  options: readonly SelectOption[];
  value?: string;
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  /** Accessible name of the trigger. */
  label: string;
  id?: string;
  /** Width token (default "md"). */
  width?: FieldWidth;
}

function Chevron() {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 16 16"
      className="size-4 shrink-0 text-fg-secondary"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M4 6l4 4 4-4" />
    </svg>
  );
}

/** Radix Select: role=combobox trigger, listbox content, arrows/Home/End/typeahead/Escape built in. */
export function Select({
  options,
  value,
  defaultValue,
  onValueChange,
  placeholder,
  disabled = false,
  label,
  id,
  width = "md",
}: SelectProps) {
  return (
    <RadixSelect.Root
      {...(value !== undefined && { value })}
      {...(defaultValue !== undefined && { defaultValue })}
      {...(onValueChange && { onValueChange })}
      disabled={disabled}
    >
      <RadixSelect.Trigger
        {...(id !== undefined && { id })}
        aria-label={label}
        className={cx(
          FIELD_BASE,
          FIELD_BORDER,
          FIELD_WIDTH[width],
          "inline-flex items-center justify-between gap-2 text-left data-[placeholder]:text-fg-tertiary",
          FOCUS_RING,
          DISABLED,
        )}
      >
        <RadixSelect.Value {...(placeholder !== undefined && { placeholder })} />
        <RadixSelect.Icon asChild>
          <Chevron />
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal>
        <RadixSelect.Content
          position="popper"
          sideOffset={4}
          className="z-50 min-w-(--radix-select-trigger-width) rounded-sheet border border-stroke bg-surface-2 p-1 text-fg-primary shadow-popover"
        >
          <RadixSelect.Viewport>
            {options.map((option) => (
              <RadixSelect.Item
                key={option.value}
                value={option.value}
                disabled={option.disabled === true}
                className="flex h-8 cursor-default items-center rounded-control px-3 text-body outline-none data-[disabled]:opacity-50 data-[highlighted]:bg-accent data-[highlighted]:text-fg-on-accent"
              >
                <RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  );
}
