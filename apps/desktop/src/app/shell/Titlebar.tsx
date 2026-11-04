import { useMemo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { WindowControls } from "./WindowControls";
import { defaultWindowApi, type WindowApi } from "./windowApi";

// The toolbar of the content pane (docs/07 section 4, ADR-0035 and its M2-E1-T23 addendum):
// there is no full-width titlebar any more, the sidebar runs the full window height and this
// 44 px row sits on top of the (opaque) content only. Left: the page title; right: the Ctrl+K
// search slot, the page's optional primary action and the window controls (coloured dots).
// Everything that is not a control is the drag region (`data-tauri-drag-region="deep"`;
// Tauri's drag script skips interactive elements and maximizes/restores on double-click).
// The test id stays `titlebar`: it is still the window's caption row for the content.

const SEARCH_BUTTON = cx(
  "inline-flex h-7 min-w-36 items-center gap-2 rounded-control border border-stroke-control bg-fill px-2.5 text-callout text-fg-secondary hover:bg-fill-hover active:bg-fill-pressed",
  FOCUS_RING,
);

/** DOM event the default search slot dispatches until the command palette (M2-E1-T14) exists. */
export const OPEN_PALETTE_EVENT = "forgebox:open-palette";

export interface TitlebarProps {
  /** Page title on the left (the section name); plain text, not a heading. */
  title?: string | undefined;
  /** The page's primary action (a solid accent button), left of the window controls. */
  primaryAction?: ReactNode;
  /** Opens the command palette; the default dispatches {@link OPEN_PALETTE_EVENT}. */
  onOpenPalette?: () => void;
  /** Replaces the default search button (the palette task owns the real one). */
  searchSlot?: ReactNode;
  /** The native window (a fake in tests; inert outside Tauri). */
  windowApi?: WindowApi;
}

function SearchGlyph() {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="12"
      height="12"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    >
      <circle cx="5" cy="5" r="3.75" />
      <path d="M8 8l3 3" />
    </svg>
  );
}

function openPaletteDefault() {
  window.dispatchEvent(new CustomEvent(OPEN_PALETTE_EVENT));
}

export function Titlebar({
  title,
  primaryAction,
  onOpenPalette,
  searchSlot,
  windowApi,
}: TitlebarProps) {
  const { t } = useTranslation("shell");
  const api = useMemo(() => windowApi ?? defaultWindowApi(), [windowApi]);
  const open = onOpenPalette ?? openPaletteDefault;

  return (
    <header
      data-tauri-drag-region="deep"
      data-testid="titlebar"
      className="flex h-11 shrink-0 select-none items-center gap-2 border-b border-stroke bg-(--bg-canvas-solid) pr-2 pl-4"
    >
      <span data-testid="toolbar-title" className="min-w-0 truncate text-headline text-fg-primary">
        {title}
      </span>
      <div className="min-w-4 flex-1 self-stretch" />
      {searchSlot ?? (
        <button
          type="button"
          onClick={open}
          aria-keyshortcuts="Control+K"
          title={t("titlebar.searchHint")}
          className={SEARCH_BUTTON}
        >
          <SearchGlyph />
          <span>{t("titlebar.search")}</span>
          {/* Announced through aria-keyshortcuts, not as part of the name. */}
          <kbd aria-hidden="true" className="ml-auto font-sans text-caption text-fg-tertiary">
            {t("titlebar.shortcut")}
          </kbd>
        </button>
      )}
      {primaryAction === undefined ? null : (
        <div data-testid="toolbar-action" className="flex shrink-0 items-center">
          {primaryAction}
        </div>
      )}
      <WindowControls api={api} />
    </header>
  );
}
