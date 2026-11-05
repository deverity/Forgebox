import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, IconButton } from "../../design/primitives";
import { reopenTab, terminalsStore, type TerminalTab } from "../../state/terminals";
import { Glyph } from "./Glyph";
import { ensureEntry, startEntry, type RegistryDeps, type TerminalEntry } from "./registry";

interface SearchBarProps {
  entry: TerminalEntry;
  onClose: () => void;
}

/** Ctrl+Shift+F: incremental search with the xterm search addon. */
function SearchBar({ entry, onClose }: SearchBarProps) {
  const { t } = useTranslation("terminal");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
  }, []);
  const { search } = entry.bundle;
  const find = (backwards: boolean) => {
    const text = input.current?.value ?? "";
    if (text === "") return;
    if (backwards) search.findPrevious(text);
    else search.findNext(text);
  };
  return (
    <div
      role="search"
      aria-label={t("search.label")}
      className="absolute top-2 right-4 z-10 flex items-center gap-1 rounded-control border border-stroke bg-surface px-2 py-1 shadow-popover"
    >
      <input
        ref={input}
        type="text"
        aria-label={t("search.label")}
        placeholder={t("search.placeholder")}
        className="w-48 bg-transparent text-callout text-fg-primary outline-none"
        onChange={() => {
          find(false);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            find(event.shiftKey);
          } else if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      />
      <IconButton
        label={t("search.previous")}
        icon={<Glyph name="up" />}
        onClick={() => {
          find(true);
        }}
      />
      <IconButton
        label={t("search.next")}
        icon={<Glyph name="down" />}
        onClick={() => {
          find(false);
        }}
      />
      <IconButton label={t("search.close")} icon={<Glyph name="cross" />} onClick={onClose} />
    </div>
  );
}

export interface TerminalPaneProps {
  tab: TerminalTab;
  active: boolean;
  deps: RegistryDeps;
}

/** One tab's emulator. Inactive panes stay laid out (invisible) so they fit and keep streaming. */
export function TerminalPane({ tab, active, deps }: TerminalPaneProps) {
  const { t } = useTranslation("terminal");
  const host = useRef<HTMLDivElement>(null);
  const [entry, setEntry] = useState<TerminalEntry | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    const element = host.current;
    if (element === null) return undefined;
    let cancelled = false;
    let observer: ResizeObserver | null = null;
    let frame = 0;
    void ensureEntry(tab, element, deps).then(async (created) => {
      if (cancelled) return;
      created.openSearch = () => {
        setSearching(true);
      };
      setEntry(created);
      const refit = () => {
        cancelAnimationFrame(frame);
        frame = requestAnimationFrame(() => {
          try {
            created.bundle.fit.fit();
          } catch {
            /* zero-sized while hidden */
          }
        });
      };
      observer = new ResizeObserver(refit);
      observer.observe(element);
      try {
        created.bundle.fit.fit();
      } catch {
        /* measured by the observer once laid out */
      }
      const latest = terminalsStore.getState().tabs.find((x) => x.key === tab.key) ?? tab;
      await startEntry(created, latest);
    });
    return () => {
      cancelled = true;
      cancelAnimationFrame(frame);
      observer?.disconnect();
    };
    // The deps object is rebuilt per render; the tab identity and epoch decide.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the deps object is rebuilt per render
  }, [tab.key, tab.epoch]);

  useEffect(() => {
    if (!active || entry === null) return;
    try {
      entry.bundle.fit.fit();
    } catch {
      /* not laid out yet */
    }
    entry.bundle.term.focus();
  }, [active, entry]);

  const closeSearch = () => {
    setSearching(false);
    entry?.bundle.search.clearDecorations();
    entry?.bundle.term.focus();
  };

  return (
    <div
      data-testid={`terminal-pane-${tab.key}`}
      data-active={active}
      className={
        active
          ? "absolute inset-0 flex flex-col"
          : "pointer-events-none invisible absolute inset-0 flex flex-col"
      }
    >
      {/* The focus lives in xterm's hidden textarea: the ring is on the host while it is inside
          (docs/07 section 9, no focus without a visible ring). */}
      <div
        ref={host}
        className="min-h-0 flex-1 bg-surface p-2 focus-within:outline-2 focus-within:-outline-offset-2 focus-within:outline-accent focus-within:outline-solid"
      />
      {searching && entry !== null ? <SearchBar entry={entry} onClose={closeSearch} /> : null}
      {tab.status === "opening" && tab.terminalId === null && tab.error === null ? (
        <p role="status" className="absolute top-3 left-4 text-callout text-fg-secondary">
          {t("opening")}
        </p>
      ) : null}
      {tab.status === "failed" ? (
        <div
          role="alert"
          className="flex items-center gap-3 border-t border-stroke bg-surface-2 px-4 py-2"
        >
          <span className="flex-1 text-callout text-state-failed">
            {t("failed", { message: tab.error ?? "" })}
          </span>
          <Button
            onClick={() => {
              reopenTab(tab.key);
            }}
          >
            {t("retry")}
          </Button>
        </div>
      ) : null}
      {tab.exit !== null ? (
        <div
          role="status"
          data-testid="terminal-exited"
          className="flex items-center gap-3 border-t border-stroke bg-surface-2 px-4 py-2"
        >
          <span className="flex-1 text-callout text-fg-secondary">
            {tab.exit.code === null ? t("exitedNoCode") : t("exited", { code: tab.exit.code })}
          </span>
          <Button
            variant="primary"
            onClick={() => {
              reopenTab(tab.key);
            }}
          >
            {t("reopen")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
