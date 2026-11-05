import type { ComponentPropsWithoutRef } from "react";

export interface CardProps extends ComponentPropsWithoutRef<"div"> {
  /** Inner padding on the 4 px scale. */
  padding?: "none" | "md" | "lg";
}

const PADDING = { none: "", md: "p-4", lg: "p-6" } as const;

/** Border plus surface; cards never carry a shadow (docs/07 section 2.5). */
export function Card({ padding = "md", className, ...rest }: CardProps) {
  const classes = [
    "rounded-card border border-stroke bg-surface text-fg-primary",
    PADDING[padding],
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return <div className={classes} {...rest} />;
}
