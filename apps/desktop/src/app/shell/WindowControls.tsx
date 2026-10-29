import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { WindowApi } from "./windowApi";

// Minimize / maximize-restore / close, drawn like the Windows caption buttons (46x32, 10 px
// glyphs). The tooltip is the native one (`title`), as on Windows' own caption buttons; the
// accessible name is `aria-label`. Buttons block the drag region (Tauri's drag script skips
// clickable elements), so double-click on them never maximizes.

const BUTTON =
  "inline-flex h-8 w-11.5 shrink-0 items-center justify-center text-fg-primary outline-none " +
  "hover:bg-stroke focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent";
const CLOSE = `${BUTTON} hover:bg-state-failed hover:text-surface`;

function Glyph({ children }: { children: ReactNode }) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="10"
      height="10"
      viewBox="0 0 10 10"
      fill="none"
      stroke="currentColor"
      strokeWidth="1"
      shapeRendering="crispEdges"
    >
      {children}
    </svg>
  );
}

/** Tracks the maximized state of the window (restore glyph when maximized). */
function useMaximized(api: WindowApi): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    let alive = true;
    let stop: (() => void) | undefined;
    api.isMaximized().then(
      (value) => {
        if (alive) setMaximized(value);
      },
      () => undefined,
    );
    api
      .onMaximizedChange((value) => {
        if (alive) setMaximized(value);
      })
      .then(
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
  }, [api]);
  return maximized;
}

export interface WindowControlsProps {
  api: WindowApi;
}

export function WindowControls({ api }: WindowControlsProps) {
  const { t } = useTranslation("shell");
  const maximized = useMaximized(api);
  const maximizeLabel = maximized ? t("titlebar.restore") : t("titlebar.maximize");

  return (
    <div className="flex h-8 items-stretch" role="group" aria-label={t("titlebar.windowControls")}>
      <button
        type="button"
        className={BUTTON}
        aria-label={t("titlebar.minimize")}
        title={t("titlebar.minimize")}
        onClick={() => void api.minimize()}
      >
        <Glyph>
          <path d="M0 5.5h10" />
        </Glyph>
      </button>
      <button
        type="button"
        className={BUTTON}
        aria-label={maximizeLabel}
        title={maximizeLabel}
        data-maximized={maximized ? "true" : "false"}
        onClick={() => void api.toggleMaximize()}
      >
        {maximized ? (
          <Glyph>
            <path d="M2.5 2.5V0.5h7v7h-2" />
            <rect x="0.5" y="2.5" width="7" height="7" />
          </Glyph>
        ) : (
          <Glyph>
            <rect x="0.5" y="0.5" width="9" height="9" />
          </Glyph>
        )}
      </button>
      <button
        type="button"
        className={CLOSE}
        aria-label={t("titlebar.close")}
        title={t("titlebar.close")}
        onClick={() => void api.close()}
      >
        <Glyph>
          <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" shapeRendering="geometricPrecision" />
        </Glyph>
      </button>
    </div>
  );
}
