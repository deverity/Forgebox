import { forwardRef } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { DISABLED, FOCUS_RING, SPINNER, cx } from "./shared";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "destructive";

export const BUTTON_VARIANTS: readonly ButtonVariant[] = [
  "primary",
  "secondary",
  "ghost",
  "destructive",
];

export const BUTTON_VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary: "bg-accent text-fg-on-accent border-transparent hover:opacity-90 active:opacity-80",
  secondary:
    "bg-surface text-fg-primary border-stroke-strong hover:bg-surface-2 active:bg-surface-2",
  ghost: "bg-transparent text-fg-primary border-transparent hover:bg-surface-2 active:bg-surface-2",
  destructive:
    "bg-transparent text-state-failed border-state-failed hover:bg-surface-2 active:bg-surface-2",
};

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  variant?: ButtonVariant;
  /** Shows a spinner and blocks clicks; the width does not change (no layout shift, docs/07 section 11). */
  loading?: boolean;
  children: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", loading = false, disabled, type = "button", children, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      {...rest}
      disabled={disabled === true || loading}
      aria-busy={loading || undefined}
      data-variant={variant}
      data-loading={loading || undefined}
      className={cx(
        "relative inline-flex h-8 items-center justify-center gap-2 rounded-control border px-4 text-body transition-colors duration-(--duration-micro)",
        BUTTON_VARIANT_CLASS[variant],
        FOCUS_RING,
        DISABLED,
        loading && "disabled:cursor-progress disabled:opacity-100",
      )}
    >
      {/* The label stays in flow (invisible while loading) so the button keeps its width. */}
      <span className={cx("inline-flex items-center gap-2", loading && "invisible")}>
        {children}
      </span>
      {loading ? (
        <span aria-hidden="true" data-testid="button-spinner" className={SPINNER} />
      ) : null}
    </button>
  );
});
