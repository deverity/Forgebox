import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import type { WindowApi } from "./windowApi";

// The window controls as coloured dots on the RIGHT, in Windows order (user decision
// 2026-11-04, docs/07 section 4, ADR-0035 addendum M2-E1-T23): minimize (yellow),
// maximize/restore (green), close (red). 12 px dots 8 px apart; each button is a 24x24 hit
// target (neighbours overlap by 4 px so the dots keep their 8 px gap). Glyphs show while the
// group is hovered or a dot has keyboard focus; all three turn grey while the window is not
// focused (Tauri focus events) and stay grey on hover, as in the approved mockup. The tooltip
// is the native one (`title`), the accessible name `aria-label`. Buttons block the drag region
// (Tauri's drag script skips clickable elements), so double-click on them never maximizes.

/** One hit target: 24x24, round, with the visible focus ring. */
const HIT = cx(
  "relative inline-flex size-6 shrink-0 items-center justify-center rounded-full",
  FOCUS_RING,
);

/** The dot itself; its colour comes from {@link COLOR} and turns grey while inactive. */
const DOT =
  "flex size-3 items-center justify-center rounded-full ring-[0.5px] ring-inset ring-window-dot-stroke " +
  "group-data-[focused=false]/dots:bg-window-inactive forced-colors:forced-color-adjust-none";

/** The glyph: hidden until the group is hovered or a dot has focus; never while inactive. */
const GLYPH =
  "text-window-glyph opacity-0 group-hover/dots:opacity-100 group-has-[:focus-visible]/dots:opacity-100 " +
  "group-data-[focused=false]/dots:opacity-0 motion-safe:transition-opacity motion-safe:duration-(--duration-micro) " +
  "forced-colors:opacity-100";

export type WindowControlId = "minimize" | "maximize" | "close";

/** DOM order = visual order = Tab order (Windows order, on the right). */
export const WINDOW_CONTROL_ORDER: readonly WindowControlId[] = ["minimize", "maximize", "close"];

/** Token colour of each dot (docs/design/tokens.json `window-*`). */
export const WINDOW_CONTROL_COLOR: Record<WindowControlId, string> = {
  minimize: "bg-window-minimize",
  maximize: "bg-window-maximize",
  close: "bg-window-close",
};

function Glyph({ children }: { children: ReactNode }) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="8"
      height="8"
      viewBox="0 0 8 8"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={GLYPH}
    >
      {children}
    </svg>
  );
}

type Read = (api: WindowApi) => Promise<boolean>;
type Follow = (api: WindowApi, onChange: (value: boolean) => void) => Promise<() => void>;

const readMaximized: Read = (api) => api.isMaximized();
const followMaximized: Follow = (api, onChange) => api.onMaximizedChange(onChange);
const readFocused: Read = (api) => api.isFocused();
const followFocused: Follow = (api, onChange) => api.onFocusChange(onChange);

/** Follows a boolean window fact: the first answer, then every change; never rejects. */
function useWindowFlag(api: WindowApi, initial: boolean, read: Read, follow: Follow): boolean {
  const [value, setValue] = useState(initial);
  useEffect(() => {
    let alive = true;
    let stop: (() => void) | undefined;
    read(api).then(
      (v) => {
        if (alive) setValue(v);
      },
      () => undefined,
    );
    follow(api, (v) => {
      if (alive) setValue(v);
    }).then(
      (unlisten) => {
        if (alive) stop = unlisten;
        else unlisten();
      },
      () => undefined,
    );
    return () => {
      alive = false;
      stop?.();
    };
  }, [api, read, follow]);
  return value;
}

export interface WindowControlsProps {
  api: WindowApi;
}

export function WindowControls({ api }: WindowControlsProps) {
  const { t } = useTranslation("shell");
  const maximized = useWindowFlag(api, false, readMaximized, followMaximized);
  // Starts focused: the window is shown and focused by `ready`.
  const focused = useWindowFlag(api, true, readFocused, followFocused);
  const label: Record<WindowControlId, string> = {
    minimize: t("titlebar.minimize"),
    maximize: maximized ? t("titlebar.restore") : t("titlebar.maximize"),
    close: t("titlebar.close"),
  };
  const act: Record<WindowControlId, () => Promise<void>> = {
    minimize: () => api.minimize(),
    maximize: () => api.toggleMaximize(),
    close: () => api.close(),
  };
  const glyph: Record<WindowControlId, ReactNode> = {
    minimize: (
      <Glyph>
        <path d="M1.5 4h5" />
      </Glyph>
    ),
    maximize: maximized ? (
      <Glyph>
        <path d="M3 1.5h3.5V5" />
        <rect x="1.5" y="3" width="3.5" height="3.5" />
      </Glyph>
    ) : (
      <Glyph>
        <rect x="1.75" y="1.75" width="4.5" height="4.5" />
      </Glyph>
    ),
    close: (
      <Glyph>
        <path d="M2 2l4 4M6 2L2 6" />
      </Glyph>
    ),
  };

  return (
    <div
      role="group"
      aria-label={t("titlebar.windowControls")}
      data-testid="window-controls"
      data-focused={String(focused)}
      className="group/dots ml-1 flex shrink-0 items-center -space-x-1"
    >
      {WINDOW_CONTROL_ORDER.map((id) => (
        <button
          key={id}
          type="button"
          className={HIT}
          aria-label={label[id]}
          title={label[id]}
          data-control={id}
          {...(id === "maximize" ? { "data-maximized": String(maximized) } : {})}
          onClick={() => void act[id]()}
        >
          <span
            aria-hidden="true"
            data-testid={`dot-${id}`}
            className={cx(DOT, WINDOW_CONTROL_COLOR[id])}
          >
            {glyph[id]}
          </span>
        </button>
      ))}
    </div>
  );
}
