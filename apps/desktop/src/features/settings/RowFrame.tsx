import type { ReactNode } from "react";

export interface RowFrameProps {
  id: string;
  label: string;
  description?: string;
  /** `forge` command or key shown under the description, in mono. */
  caption?: string;
  children: ReactNode;
}

/** Layout of a row that is not a `forgebox.toml` key (GUI prefs, read-only facts, links). */
export function RowFrame({ id, label, description, caption, children }: RowFrameProps) {
  return (
    <div
      data-testid={`setting-${id}`}
      className="flex flex-wrap items-start justify-between gap-3 border-b border-stroke py-3"
    >
      <div className="flex min-w-48 flex-1 flex-col gap-1">
        <span className="text-body">{label}</span>
        {description === undefined ? null : (
          <p className="text-callout text-fg-secondary">{description}</p>
        )}
        {caption === undefined ? null : (
          <code className="text-caption text-fg-tertiary">{caption}</code>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2">{children}</div>
    </div>
  );
}
