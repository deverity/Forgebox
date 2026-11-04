import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LogViewer } from "./LogViewer";
import { TargetPicker } from "./TargetPicker";

/** Ctrl+L focuses the screen (also when already here). */
function useFocusShortcut(focus: () => void) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "l") {
        focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [focus]);
}

/** `/logs` (docs/07 section 5.8): pick sources, follow them merged, filter, copy, open files. */
export function LogsPage() {
  const { t } = useTranslation("logs");
  const navigate = useNavigate();
  const fromUrl = useSearch({
    strict: false,
    select: (search: Record<string, unknown>) =>
      typeof search.target === "string" && search.target !== "" ? search.target : undefined,
  });
  const [selected, setSelected] = useState<string[]>(fromUrl === undefined ? [] : [fromUrl]);
  // The screen takes the focus itself (not a text field: those swallow the global shortcuts).
  const screen = useRef<HTMLDivElement>(null);
  useFocusShortcut(() => {
    screen.current?.focus();
  });
  useEffect(() => {
    screen.current?.focus();
  }, []);

  const change = (next: string[]) => {
    setSelected(next);
    // Keeps the first source in the URL so the screen can be reopened from a link.
    void navigate({
      to: "/logs",
      search: next[0] === undefined ? {} : { target: next[0] },
      replace: true,
    });
  };

  return (
    <div
      ref={screen}
      tabIndex={-1}
      data-testid="page-logs"
      className="flex min-h-0 flex-1 flex-col overflow-hidden outline-hidden"
    >
      <header className="px-4 pt-4 pb-2">
        <h1 className="text-title-1">{t("title")}</h1>
      </header>
      <div className="flex min-h-0 flex-1 gap-4 px-4 pb-4">
        <aside className="flex min-h-0 w-64 shrink-0 flex-col">
          <TargetPicker selected={selected} onChange={change} />
        </aside>
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <LogViewer targets={selected} name={selected.join(", ")} />
        </div>
      </div>
    </div>
  );
}
