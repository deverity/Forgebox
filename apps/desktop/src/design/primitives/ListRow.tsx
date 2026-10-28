import type { ReactNode } from "react";

export interface ListRowProps {
  title: ReactNode;
  subtitle?: ReactNode;
  /** Icon or StatusDot, 20 px. */
  leading?: ReactNode;
  trailing?: ReactNode;
  selected?: boolean;
  /** When set the row is a real button (keyboard + focus ring). */
  onClick?: () => void;
  className?: string;
}

const BASE = "flex w-full items-center gap-3 px-3 text-left text-body";

/** Fixed 48 px row: leading, two text lines, trailing slot. Height never depends on content. */
export function ListRow({
  title,
  subtitle,
  leading,
  trailing,
  selected = false,
  onClick,
  className,
}: ListRowProps) {
  const classes = [
    BASE,
    "h-12 rounded-control",
    selected ? "bg-surface-2" : "",
    onClick
      ? "cursor-pointer hover:bg-surface-2 focus-visible:outline-2 focus-visible:outline-accent"
      : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  const content = (
    <>
      {leading ? (
        <span className="flex size-5 shrink-0 items-center justify-center">{leading}</span>
      ) : null}
      <span className="min-w-0 flex-1">
        <span className="block truncate text-fg-primary">{title}</span>
        {subtitle ? (
          <span className="block truncate text-caption text-fg-secondary">{subtitle}</span>
        ) : null}
      </span>
      {trailing ? (
        <span className="shrink-0 text-callout text-fg-secondary">{trailing}</span>
      ) : null}
    </>
  );
  if (onClick) {
    return (
      <button
        type="button"
        className={classes}
        onClick={onClick}
        aria-pressed={selected || undefined}
      >
        {content}
      </button>
    );
  }
  return <div className={classes}>{content}</div>;
}
