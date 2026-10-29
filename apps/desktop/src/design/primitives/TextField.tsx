import { forwardRef, useId, useState } from "react";
import type { ChangeEvent, InputHTMLAttributes } from "react";
import { DISABLED, FIELD_BASE, FIELD_BORDER, FIELD_WIDTH, FOCUS_RING, cx } from "./shared";
import type { FieldWidth } from "./shared";

export interface TextFieldProps extends Omit<
  InputHTMLAttributes<HTMLInputElement>,
  "className" | "id" | "aria-label"
> {
  /** Visible label (already localized by the caller). */
  label: string;
  hint?: string;
  /** Width token (default "md"). */
  width?: FieldWidth;
  /** Forced error message (e.g. from the server); wins over `validate`. */
  error?: string;
  /**
   * Inline validation: returns a localized message or undefined. Runs on blur and then on
   * every change, so the message appears after the first visit and clears as soon as it is fixed.
   */
  validate?: (value: string) => string | undefined;
}

export const TextField = forwardRef<HTMLInputElement, TextFieldProps>(function TextField(
  {
    label,
    hint,
    width = "md",
    error,
    validate,
    value,
    defaultValue,
    onChange,
    onBlur,
    disabled,
    ...rest
  },
  ref,
) {
  const uid = useId();
  const [inner, setInner] = useState(String(defaultValue ?? ""));
  const [touched, setTouched] = useState(false);
  const current = value === undefined ? inner : String(value);
  const message = error ?? (touched && validate ? validate(current) : undefined);
  const describedBy = [message ? `${uid}-error` : null, hint ? `${uid}-hint` : null]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={cx("flex flex-col gap-1", FIELD_WIDTH[width])}>
      <label htmlFor={uid} className="text-callout text-fg-secondary">
        {label}
      </label>
      <input
        ref={ref}
        {...rest}
        id={uid}
        value={current}
        disabled={disabled}
        aria-invalid={message ? true : undefined}
        aria-describedby={describedBy === "" ? undefined : describedBy}
        onChange={(event: ChangeEvent<HTMLInputElement>) => {
          setInner(event.target.value);
          onChange?.(event);
        }}
        onBlur={(event) => {
          setTouched(true);
          onBlur?.(event);
        }}
        className={cx(
          FIELD_BASE,
          "w-full",
          // Exactly one border colour (see FIELD_BASE).
          message ? "border-state-failed" : FIELD_BORDER,
          FOCUS_RING,
          DISABLED,
        )}
      />
      {message ? (
        <p id={`${uid}-error`} role="alert" className="text-caption text-state-failed">
          {message}
        </p>
      ) : null}
      {hint ? (
        <p id={`${uid}-hint`} className="text-caption text-fg-tertiary">
          {hint}
        </p>
      ) : null}
    </div>
  );
});
