import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { NEW_SITE_EVENT, takePendingNewSite } from "../../app/palette/actions";
import { Badge } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { Kbd } from "../../design/primitives/Kbd";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { Skeleton } from "../../design/primitives/Skeleton";
import { VirtualList } from "../../design/primitives/VirtualList";
import type * as T from "../../ipc/generated";
import { useConnectionStatus, useRuntime, useSites } from "../../state";
import { useStore } from "zustand";
import { CreateSiteSheet } from "./CreateSiteSheet";
import { filterSites, indexSites } from "./model";
import { RemoveSiteDialog } from "./RemoveSiteDialog";
import { CARD_HEIGHT, ROW_HEIGHT, SiteCard, type SitesLayout } from "./SiteCard";

const EMPTY_KEY = "empty";
const SITES_PATH = "/sites";
const LAYOUT_GRID: SitesLayout = "grid";
const LAYOUT_LIST: SitesLayout = "list";
const NEW_CHORD = "Ctrl+N";
const LAYOUT_KEY = "forgebox.sites.layout";
const MIN_CARD_WIDTH = 300;
const MAX_COLUMNS = 4;
const FALLBACK_COLUMNS = 3;

function loadLayout(): SitesLayout {
  try {
    return globalThis.localStorage.getItem(LAYOUT_KEY) === "list" ? "list" : "grid";
  } catch {
    return "grid";
  }
}

function saveLayout(layout: SitesLayout): void {
  try {
    globalThis.localStorage.setItem(LAYOUT_KEY, layout);
  } catch {
    // Not persisted (private mode, blocked storage): the choice still holds for this session.
  }
}

/** How many cards fit side by side; measured on the list container. */
function useColumns(): [React.RefObject<HTMLDivElement | null>, number] {
  const ref = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(FALLBACK_COLUMNS);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null) return undefined;
    const measure = () => {
      const width = element.clientWidth;
      if (width <= 0) return;
      setColumns(Math.max(1, Math.min(MAX_COLUMNS, Math.floor(width / MIN_CARD_WIDTH))));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, []);
  return [ref, columns];
}

function chunk<T>(items: readonly T[], size: number): T[][] {
  const rows: T[][] = [];
  for (let i = 0; i < items.length; i += size) rows.push(items.slice(i, i + size));
  return rows;
}

/** /sites (docs/07 section 5.4): grid or list, instant search, create, remove. */
export function SitesPage() {
  const { t } = useTranslation("sites");
  const runtime = useRuntime();
  const sites = useSites();
  const loaded = useStore(runtime.stores.sites, (state) => state.loaded);
  const connection = useConnectionStatus();

  const [query, setQuery] = useState("");
  // `/sites?php=8.4` (the PHP screen's sites count) keeps only the sites of that series.
  const navigate = useNavigate();
  const phpFilter = useSearch({
    strict: false,
    select: (search: Record<string, unknown>) =>
      typeof search.php === "string" && search.php !== "" ? search.php : undefined,
  });
  const [layout, setLayout] = useState<SitesLayout>(loadLayout);
  // A "new site" request made before this screen mounted (lazy chunk) opens the sheet at once.
  const [creating, setCreating] = useState(takePendingNewSite);
  const [removing, setRemoving] = useState<T.SiteDto | null>(null);
  const [listRef, columns] = useColumns();

  // Ctrl+N and the palette's "New site" (a request made before this screen mounted counts).
  useEffect(() => {
    const open = () => {
      takePendingNewSite();
      setCreating(true);
    };
    window.addEventListener(NEW_SITE_EVENT, open);
    return () => {
      window.removeEventListener(NEW_SITE_EVENT, open);
    };
  }, []);

  const index = useMemo(
    () =>
      indexSites(sites).filter(
        (entry) => phpFilter === undefined || entry.site.php?.series === phpFilter,
      ),
    [sites, phpFilter],
  );
  const matches = useMemo(() => filterSites(index, query), [index, query]);
  const perRow = layout === "grid" ? columns : 1;
  const rows = useMemo(
    () =>
      chunk(
        matches.map((entry) => entry.site),
        perRow,
      ),
    [matches, perRow],
  );

  const chooseLayout = (next: string) => {
    const value: SitesLayout = next === "list" ? "list" : "grid";
    setLayout(value);
    saveLayout(value);
  };

  const total = index.length;
  const trimmed = query.trim();
  let body;
  if (!loaded && connection !== "connected") {
    body = <EmptyState title={t("empty.title")} description={t("empty.description")} />;
  } else if (!loaded) {
    body = (
      <div className="flex flex-col gap-2 p-4" aria-busy="true">
        <Skeleton height={ROW_HEIGHT} />
        <Skeleton height={ROW_HEIGHT} />
        <Skeleton height={ROW_HEIGHT} />
      </div>
    );
  } else if (total === 0) {
    body = (
      <EmptyState
        title={t("empty.title")}
        description={t("empty.description")}
        action={
          <Button
            variant="primary"
            onClick={() => {
              setCreating(true);
            }}
          >
            {t("new")}
          </Button>
        }
      />
    );
  } else if (matches.length === 0) {
    body = (
      <EmptyState
        title={t("empty.noMatchTitle", { query: trimmed })}
        description={t("empty.noMatchDescription")}
      />
    );
  } else {
    body = (
      <VirtualList
        key={layout}
        items={rows}
        label={t("list.label")}
        rowHeight={layout === "grid" ? CARD_HEIGHT : ROW_HEIGHT}
        overscan={2}
        initialHeight={600}
        getKey={(row) => row[0]?.id ?? EMPTY_KEY}
        renderRow={(row) => (
          <div
            className="grid gap-2 px-4 pb-2"
            style={{ gridTemplateColumns: `repeat(${String(perRow)}, minmax(0, 1fr))` }}
          >
            {row.map((site) => (
              <SiteCard key={site.id} site={site} layout={layout} onRemove={setRemoving} />
            ))}
          </div>
        )}
      />
    );
  }

  return (
    <div data-testid="page-sites" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <span className="text-callout text-fg-secondary" aria-live="polite">
          {trimmed === ""
            ? t("count", { count: total })
            : t("countFiltered", { shown: matches.length, total })}
        </span>
        {phpFilter === undefined ? null : (
          <span className="inline-flex items-center gap-1" data-testid="php-filter">
            <Badge tone="accent">{t("phpFilter.label", { series: phpFilter })}</Badge>
            <Button
              variant="ghost"
              onClick={() => {
                void navigate({ to: SITES_PATH, search: {}, replace: true });
              }}
            >
              {t("phpFilter.clear")}
            </Button>
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          <input
            type="search"
            aria-label={t("search.label")}
            placeholder={t("search.placeholder")}
            value={query}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setQuery(event.target.value);
            }}
            className="h-8 w-72 rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          />
          <SegmentedControl
            label={t("view.label")}
            value={layout}
            onValueChange={chooseLayout}
            options={[
              { value: LAYOUT_GRID, label: t("view.grid") },
              { value: LAYOUT_LIST, label: t("view.list") },
            ]}
          />
          <Button
            variant="primary"
            onClick={() => {
              setCreating(true);
            }}
          >
            {t("new")}
            <Kbd>{NEW_CHORD}</Kbd>
          </Button>
        </div>
      </div>
      <div ref={listRef} className="min-h-0 flex-1">
        {body}
      </div>
      <CreateSiteSheet open={creating} onOpenChange={setCreating} />
      <RemoveSiteDialog
        site={removing}
        onClose={() => {
          setRemoving(null);
        }}
      />
    </div>
  );
}
