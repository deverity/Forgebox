import { forwardRef } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { DISABLED, FOCUS_RING, SPINNER_INLINE, cx } from "./shared";

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
  /** Shows a spinner plus text and blocks clicks; the width does not change (docs/07 sections 1.2, 11). */
  loading?: boolean;
  /** Localized text shown while loading (e.g. "Saving"); defaults to the normal children. */
  loadingLabel?: string;
  children: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = "secondary",
    loading = false,
    loadingLabel,
    disabled,
    type = "button",
    children,
    ...rest
  },
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
        // The loading button is disabled too; it keeps full opacity and shows a progress cursor.
        loading ? "disabled:cursor-progress" : DISABLED,
      )}
    >
      {/* Both contents share ONE grid cell, so the width is the max of the two (no layout shift);
          the inactive one is transparent + aria-hidden, so the accessible name is exactly the
          visible text in either state. */}
      <span className="grid">
        <span
          data-testid="button-content"
          aria-hidden={loading || undefined}
          className={cx(
            "col-start-1 row-start-1 inline-flex items-center justify-center gap-2",
            loading && "opacity-0",
          )}
        >
          {children}
        </span>
        <span
          data-testid="button-loading-content"
          aria-hidden={!loading || undefined}
          className={cx(
            "col-start-1 row-start-1 inline-flex items-center justify-center gap-2",
            !loading && "opacity-0",
          )}
        >
          <span aria-hidden="true" data-testid="button-spinner" className={SPINNER_INLINE} />
          {loadingLabel ?? children}
        </span>
      </span>
    </button>
  );
});
