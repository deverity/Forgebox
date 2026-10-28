import { useMemo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { WindowControls } from "./WindowControls";
import { defaultWindowApi, type WindowApi } from "./windowApi";

// The custom titlebar (docs/07 section 4, ADR-0035): 32 px, a drag region over everything that
// is not a control (`data-tauri-drag-region="deep"`; Tauri's drag script also maximizes on
// double-click), the Ctrl+K search slot and the window controls. Transparent: the window
// material (data-vibrancy="on") or the opaque canvas shows through.

/** DOM event the default search slot dispatches until the command palette (M2-E1-T14) exists. */
export const OPEN_PALETTE_EVENT = "forgebox:open-palette";

export interface TitlebarProps {
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

export function Titlebar({ onOpenPalette, searchSlot, windowApi }: TitlebarProps) {
  const { t } = useTranslation("shell");
  const api = useMemo(() => windowApi ?? defaultWindowApi(), [windowApi]);
  const open = onOpenPalette ?? openPaletteDefault;

  return (
    <header
      data-tauri-drag-region="deep"
      data-testid="titlebar"
      className="flex h-8 shrink-0 select-none items-center bg-transparent"
    >
      <span className="pl-3 text-callout font-semibold text-fg-primary">Forgebox</span>
      <div className="min-w-4 flex-1 self-stretch" />
      {searchSlot ?? (
        <button
          type="button"
          onClick={open}
          aria-keyshortcuts="Control+K"
          aria-label={t("titlebar.searchHint")}
          title={t("titlebar.searchHint")}
          className="mr-2 inline-flex h-6 items-center gap-2 rounded-control border border-stroke px-2 text-callout text-fg-secondary outline-none hover:bg-stroke focus-visible:outline-2 focus-visible:outline-accent"
        >
          <SearchGlyph />
          <span>{t("titlebar.search")}</span>
          <kbd className="font-sans text-caption text-fg-tertiary">{t("titlebar.shortcut")}</kbd>
        </button>
      )}
      <WindowControls api={api} />
    </header>
  );
}
