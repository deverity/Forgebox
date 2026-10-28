import { forwardRef } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { BUTTON_VARIANT_CLASS } from "./Button";
import type { ButtonVariant } from "./Button";
import { DISABLED, FOCUS_RING, SPINNER, cx } from "./shared";

export interface IconButtonProps extends Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "className" | "children" | "aria-label"
> {
  /** Accessible name; mandatory because the button has no visible text. */
  label: string;
  icon: ReactNode;
  variant?: ButtonVariant;
  loading?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, icon, variant = "ghost", loading = false, disabled, type = "button", ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      title={label}
      {...rest}
      aria-label={label}
      aria-busy={loading || undefined}
      disabled={disabled === true || loading}
      data-variant={variant}
      className={cx(
        "relative inline-flex size-8 items-center justify-center rounded-control border transition-colors duration-(--duration-micro)",
        BUTTON_VARIANT_CLASS[variant],
        FOCUS_RING,
        DISABLED,
        loading && "disabled:cursor-progress disabled:opacity-100",
      )}
    >
      <span aria-hidden="true" className={cx("inline-flex size-4", loading && "invisible")}>
        {icon}
      </span>
      {loading ? <span aria-hidden="true" className={SPINNER} /> : null}
    </button>
  );
});
