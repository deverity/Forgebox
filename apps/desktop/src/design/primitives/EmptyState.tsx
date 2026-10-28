import type { ReactNode } from "react";

export interface EmptyStateProps {
  title: string;
  description?: string;
  /** Illustration slot (SVG, 96 px max). Decorative: hidden from assistive tech. */
  illustration?: ReactNode;
  /** Primary action, normally a Button. */
  action?: ReactNode;
  className?: string;
}

/** Centered "nothing here yet" block: illustration, cause in plain words, one action. */
export function EmptyState({
  title,
  description,
  illustration,
  action,
  className,
}: EmptyStateProps) {
  return (
    <div
      className={["flex flex-col items-center justify-center gap-3 p-8 text-center", className]
        .filter(Boolean)
        .join(" ")}
    >
      {illustration ? (
        <div data-slot="illustration" aria-hidden="true" className="max-h-24 text-fg-tertiary">
          {illustration}
        </div>
      ) : null}
      <h3 className="text-headline text-fg-primary">{title}</h3>
      {description ? (
        <p className="max-w-80 text-callout text-fg-secondary">{description}</p>
      ) : null}
      {action ? <div data-slot="action">{action}</div> : null}
    </div>
  );
}
