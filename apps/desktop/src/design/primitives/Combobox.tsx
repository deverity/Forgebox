import { useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { DISABLED, FIELD_BASE, FIELD_BORDER, FIELD_WIDTH, FOCUS_RING, cx } from "./shared";
import type { FieldWidth } from "./shared";

export interface ComboboxOption {
  value: string;
  label: string;
}

export interface ComboboxProps {
  options: readonly ComboboxOption[];
  value?: string | null;
  defaultValue?: string | null;
  onValueChange?: (value: string | null) => void;
  placeholder?: string;
  /** Text shown (by the caller, localized) when the filter matches nothing. */
  emptyText: string;
  disabled?: boolean;
  /** Accessible name of the input. */
  label: string;
  id?: string;
  /** Width token (default "md"). */
  width?: FieldWidth;
}

/**
 * WAI-ARIA editable combobox with list autocomplete (Radix has no combobox).
 * Down/Up move the active option, Enter selects, Escape closes (and clears the filter on a
 * second press), Home/End jump inside the input as usual.
 */
export function Combobox({
  options,
  value,
  defaultValue = null,
  onValueChange,
  placeholder,
  emptyText,
  disabled = false,
  label,
  id,
  width = "md",
}: ComboboxProps) {
  const uid = useId();
  const listId = `${uid}-list`;
  const [inner, setInner] = useState<string | null>(defaultValue);
  const selected = value === undefined ? inner : value;
  const selectedLabel = options.find((o) => o.value === selected)?.label ?? "";
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState<string | null>(null); // null = show the selected label
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const text = filter ?? selectedLabel;
  const visible = useMemo(() => {
    const needle = (filter ?? "").trim().toLowerCase();
    return needle === "" ? options : options.filter((o) => o.label.toLowerCase().includes(needle));
  }, [options, filter]);
  const activeIndex = Math.min(active, Math.max(visible.length - 1, 0));
  const activeOption = visible[activeIndex];

  function commit(option: ComboboxOption) {
    setInner(option.value);
    onValueChange?.(option.value);
    setFilter(null);
    setOpen(false);
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (!open) setOpen(true);
        else setActive((activeIndex + 1) % Math.max(visible.length, 1));
        break;
      case "ArrowUp":
        event.preventDefault();
        if (!open) setOpen(true);
        else setActive((activeIndex - 1 + visible.length) % Math.max(visible.length, 1));
        break;
      case "Enter":
        if (open && activeOption) {
          event.preventDefault();
          commit(activeOption);
        }
        break;
      case "Escape":
        if (open) {
          event.preventDefault();
          setOpen(false);
          setFilter(null);
        }
        break;
      default:
    }
  }

  return (
    <div className={cx("relative inline-block", FIELD_WIDTH[width])}>
      <input
        ref={inputRef}
        {...(id !== undefined && { id })}
        role="combobox"
        aria-label={label}
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={
          open && activeOption ? `${uid}-opt-${String(activeIndex)}` : undefined
        }
        autoComplete="off"
        disabled={disabled}
        placeholder={placeholder}
        value={text}
        onChange={(event) => {
          setFilter(event.target.value);
          setActive(0);
          setOpen(true);
        }}
        onFocus={(event) => {
          event.target.select();
        }}
        onBlur={() => {
          setOpen(false);
          setFilter(null);
        }}
        onKeyDown={onKeyDown}
        className={cx(FIELD_BASE, FIELD_BORDER, "w-full", FOCUS_RING, DISABLED)}
      />
      <ul
        id={listId}
        role="listbox"
        aria-label={label}
        hidden={!open}
        className="absolute z-50 mt-1 max-h-60 w-full overflow-auto rounded-sheet border border-stroke bg-surface-2 p-1 shadow-popover"
      >
        {visible.map((option, index) => (
          <li
            key={option.value}
            id={`${uid}-opt-${String(index)}`}
            role="option"
            aria-selected={option.value === selected}
            data-active={index === activeIndex || undefined}
            // mousedown (not click) so the input keeps focus and onBlur does not close first.
            onMouseDown={(event) => {
              event.preventDefault();
              commit(option);
            }}
            onMouseEnter={() => {
              setActive(index);
            }}
            className={cx(
              "flex h-8 cursor-default items-center rounded-control px-3 text-body text-fg-primary",
              index === activeIndex && "bg-accent text-fg-on-accent",
              option.value === selected && "font-semibold",
            )}
          >
            {option.label}
          </li>
        ))}
        {visible.length === 0 ? (
          <li role="presentation" className="flex h-8 items-center px-3 text-body text-fg-tertiary">
            {emptyText}
          </li>
        ) : null}
      </ul>
    </div>
  );
}
