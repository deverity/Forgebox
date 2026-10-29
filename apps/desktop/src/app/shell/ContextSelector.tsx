import { Suspense, lazy } from "react";
import { useTranslation } from "react-i18next";
import { LOCAL_CONTEXT, type ContextId } from "../../ipc/bridge";
import { CONTEXT_OPTIONS, type ContextOption } from "./contexts";

// Radix Select is heavy; the sidebar paints first with a plain box showing the same name.
const ContextSelect = lazy(() => import("./ContextSelect"));

export interface LoadedSelectProps {
  active: ContextId;
  options: readonly ContextOption[];
  nameOf: (option: ContextOption) => string;
  onSelect: (id: ContextId) => void;
}

export interface ContextSelectorProps {
  /** Icons only (sidebar below 1100 px). */
  collapsed?: boolean;
  active?: ContextId;
  options?: readonly ContextOption[];
  onSelect?: (id: ContextId) => void;
}

const BOX =
  "flex h-8 items-center rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary";

/** Top of the sidebar: the active context and the choices (Local; "Add remote" waits for M6). */
export function ContextSelector({
  collapsed = false,
  active = LOCAL_CONTEXT,
  options = CONTEXT_OPTIONS,
  onSelect,
}: ContextSelectorProps) {
  const { t } = useTranslation("shell");
  const nameOf = (o: ContextOption) => o.name ?? t(o.labelKey);
  const current = options.find((o) => o.id === active);
  const name = current === undefined ? active : nameOf(current);

  if (collapsed) {
    // One glyph with a native tooltip: the full selector needs the width the sidebar lacks.
    const label = t("context.current", { name });
    return (
      <div
        role="img"
        aria-label={label}
        title={label}
        data-testid="context-selector"
        className="mx-auto flex size-8 items-center justify-center rounded-control border border-stroke-strong text-callout font-semibold text-fg-primary"
      >
        {name.slice(0, 1).toUpperCase()}
      </div>
    );
  }
  return (
    <div data-testid="context-selector">
      <Suspense fallback={<div className={BOX}>{name}</div>}>
        <ContextSelect
          active={active}
          options={options}
          nameOf={nameOf}
          onSelect={(id) => onSelect?.(id)}
        />
      </Suspense>
    </div>
  );
}
