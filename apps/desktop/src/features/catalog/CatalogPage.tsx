import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { discoverTools } from "../../actions/tools";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { ActionButton } from "../../design/primitives/ActionButton";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { Select } from "../../design/primitives/Select";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useCapability, useConnectionStatus, useForgeQuery } from "../../state";
import { useDebounced } from "./hooks";
import {
  categoriesOf,
  matchesFilters,
  splitStacks,
  type CatalogFilters,
  type InstallFilter,
} from "./model";
import { STACK_CARD_HEIGHT, TOOL_CARD_HEIGHT, ToolCard } from "./ToolCard";

/** Typing pauses this long before `tools.search` is called. */
export const SEARCH_DEBOUNCE_MS = 250;

const ALL_CATEGORIES = "__all";
const FILTER_ALL: InstallFilter = "all";
const FILTER_INSTALLED: InstallFilter = "installed";
const FILTER_UPDATES: InstallFilter = "updates";
const SKELETON_CARDS = 6;

/** /catalog (docs/07 section 5.9): debounced search, filters, tool cards and stack cards. */
export function CatalogPage() {
  const { t } = useTranslation("catalog");
  const connection = useConnectionStatus();
  const capability = useCapability("tools.search");
  const unavailable = useCapabilityReason(capability);

  const [text, setText] = useState("");
  const [filters, setFilters] = useState<CatalogFilters>({ installed: FILTER_ALL, category: null });
  const debounced = useDebounced(text.trim(), SEARCH_DEBOUNCE_MS);

  // The category list comes from the unfiltered catalog, so choosing a category (or typing)
  // never removes the other categories from the select.
  const everything = useForgeQuery("tools.search", { text: "", category: null });
  const search = useForgeQuery("tools.search", { text: debounced, category: null });
  const categories = useMemo(() => categoriesOf(everything.data?.entries ?? []), [everything.data]);
  const visible = useMemo(
    () => (search.data?.entries ?? []).filter((entry) => matchesFilters(entry, filters)),
    [search.data, filters],
  );
  const { stacks, tools } = useMemo(() => splitStacks(visible), [visible]);

  let body;
  if (unavailable !== null && search.data === undefined) {
    body = <EmptyState title={t("unavailable")} description={unavailable} />;
  } else if (search.isError) {
    body = (
      <div className="p-4">
        <ErrorNotice
          error={search.error}
          onRetry={() => {
            void search.refetch();
          }}
        />
      </div>
    );
  } else if (search.data === undefined) {
    body =
      connection === "connected" ? (
        <div
          aria-busy="true"
          className="grid grid-cols-1 gap-3 p-4 md:grid-cols-2 xl:grid-cols-3"
          data-testid="catalog-loading"
        >
          <span className="sr-only">{t("loading")}</span>
          {Array.from({ length: SKELETON_CARDS }, (_, index) => (
            <Skeleton key={index} height={TOOL_CARD_HEIGHT} />
          ))}
        </div>
      ) : (
        <EmptyState title={t("offline.title")} description={t("offline.description")} />
      );
  } else if (visible.length === 0) {
    body = (
      <EmptyState
        title={debounced === "" ? t("empty.title") : t("empty.noMatchTitle", { query: debounced })}
        description={t("empty.description")}
      />
    );
  } else {
    body = (
      <div className="flex flex-col gap-5 p-4">
        {stacks.length > 0 ? (
          <section aria-labelledby="catalog-stacks" className="flex flex-col gap-3">
            <h2 id="catalog-stacks" className="text-title-2">
              {t("section.stacks")}
            </h2>
            <p className="text-callout text-fg-secondary">{t("section.stacksHint")}</p>
            <div
              className="grid grid-cols-1 gap-3 xl:grid-cols-2"
              style={{ minHeight: STACK_CARD_HEIGHT }}
            >
              {stacks.map((entry) => (
                <ToolCard key={entry.id} entry={entry} large />
              ))}
            </div>
          </section>
        ) : null}
        {tools.length > 0 ? (
          <section aria-labelledby="catalog-tools" className="flex flex-col gap-3">
            <h2 id="catalog-tools" className="text-title-2">
              {t("section.tools")}
            </h2>
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
              {tools.map((entry) => (
                <ToolCard key={entry.id} entry={entry} />
              ))}
            </div>
          </section>
        ) : null}
      </div>
    );
  }

  return (
    <div data-testid="page-catalog" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <span className="text-callout text-fg-secondary" aria-live="polite">
          {search.data === undefined ? "" : t("count", { count: visible.length })}
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <ActionButton
            action={discoverTools}
            loadingLabel={t("discover.running")}
            successLabel={t("discover.done")}
          >
            {t("discover.label")}
          </ActionButton>
          <input
            type="search"
            aria-label={t("search.label")}
            placeholder={t("search.placeholder")}
            value={text}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setText(event.target.value);
            }}
            className="h-8 w-72 rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          />
          <Select
            label={t("filter.category")}
            value={filters.category ?? ALL_CATEGORIES}
            onValueChange={(next) => {
              setFilters((current) => ({
                ...current,
                category: next === ALL_CATEGORIES ? null : next,
              }));
            }}
            options={[
              { value: ALL_CATEGORIES, label: t("filter.allCategories") },
              ...categories.map((category) => ({ value: category, label: category })),
            ]}
          />
          <SegmentedControl
            label={t("filter.state")}
            value={filters.installed}
            onValueChange={(next) => {
              setFilters((current) => ({
                ...current,
                installed:
                  next === FILTER_INSTALLED
                    ? FILTER_INSTALLED
                    : next === FILTER_UPDATES
                      ? FILTER_UPDATES
                      : FILTER_ALL,
              }));
            }}
            options={[
              { value: FILTER_ALL, label: t("filter.all") },
              { value: FILTER_INSTALLED, label: t("filter.installed") },
              { value: FILTER_UPDATES, label: t("filter.updates") },
            ]}
          />
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">{body}</div>
    </div>
  );
}
