import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "zustand";
import { Button } from "../../design/primitives/Button";
import { AsyncView } from "../../design/primitives/AsyncView";
import {
  useConnectionStatus,
  useInstances,
  usePools,
  useRuntime,
  useSliceError,
  useTools,
} from "../../state";
import { groupRows, type ServiceGroup } from "./model";
import { InstanceRow, PoolRow } from "./ServiceRow";
import { ServiceSheet } from "./ServiceSheet";
import { useNow } from "./useNow";

/** Pixels taken by the 44 px content toolbar and the page header around the list (no status bar). */
const CHROME_HEIGHT = 178;
const MIN_LIST_HEIGHT = 320;
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const STATUS_CONTEXT = { method: "services.status", params: {} };
const CATALOG_PATH = "/catalog";

function useListHeight(): number {
  const [height, setHeight] = useState(() => window.innerHeight - CHROME_HEIGHT);
  useEffect(() => {
    const onResize = () => {
      setHeight(window.innerHeight - CHROME_HEIGHT);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
    };
  }, []);
  return Math.max(MIN_LIST_HEIGHT, height);
}

/** The id of the instance whose sheet is open (`/services?instance=<id>`). */
function useSelected(): [string | undefined, (id: string | undefined) => void] {
  const navigate = useNavigate();
  const selected = useSearch({
    strict: false,
    select: (search: Record<string, unknown>) =>
      typeof search.instance === "string" ? search.instance : undefined,
  });
  // Stable identity: `InstanceRow` is memoized and receives this as `onOpen`, so a new function
  // per render would re-render every row on each event (found by qa:perf, event-storm).
  const select = useCallback(
    (id: string | undefined) => {
      void navigate({
        to: "/services",
        search: id === undefined ? {} : { instance: id },
        replace: true,
      });
    },
    [navigate],
  );
  return [selected, select];
}

function Groups({
  groups,
  selected,
  now,
  onOpen,
}: {
  groups: readonly ServiceGroup[];
  selected: string | undefined;
  now: number;
  onOpen: (id: string) => void;
}) {
  const { t } = useTranslation("services");
  return (
    <div className="flex flex-col gap-4 p-1">
      {groups.map((group) => (
        <section
          key={group.category}
          aria-labelledby={`group-${group.category}`}
          data-category={group.category}
        >
          <h2
            id={`group-${group.category}`}
            className="px-3 pb-1 text-caption font-semibold tracking-wide text-fg-secondary uppercase"
          >
            {t(`category.${group.category}`, { defaultValue: group.category })}
          </h2>
          <ul className="flex flex-col">
            {group.rows.map((row) =>
              row.kind === "instance" ? (
                <InstanceRow
                  key={row.key}
                  instance={row.instance}
                  selected={selected === row.key}
                  now={now}
                  onOpen={onOpen}
                />
              ) : (
                <PoolRow key={row.key} pool={row.pool} />
              ),
            )}
          </ul>
        </section>
      ))}
    </div>
  );
}

/** Route `/services` (docs/07 section 5.3): the grouped list and the detail sheet. */
export function ServicesPage() {
  const { t } = useTranslation("services");
  const runtime = useRuntime();
  const navigate = useNavigate();
  const status = useConnectionStatus();
  const instances = useInstances();
  const pools = usePools();
  const tools = useTools((state) => state.tools);
  const loaded = useStore(runtime.stores.services, (state) => state.loaded);
  const syncError = useSliceError("services");
  const [selected, select] = useSelected();
  const now = useNow();
  const height = useListHeight();

  const groups = useMemo(
    () => groupRows(Object.values(instances), Object.values(pools), tools),
    [instances, pools, tools],
  );
  // Stopped on purpose, closed or never initialized: nothing to wait for. Coming up or coming
  // back (idle, connecting, starting, reconnecting) is loading, never a "no connection" flash.
  const away = AWAY.has(status);
  const connected = status === "connected";
  const empty = groups.length === 0;
  const current = selected === undefined ? undefined : instances[selected];

  return (
    <div data-testid="page-services" className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex items-baseline justify-between gap-3 px-4 pt-4 pb-2">
        <h1 className="text-title-1">{t("title")}</h1>
        {connected && !empty ? (
          <p className="text-callout text-fg-secondary">
            {t("summary", {
              running: Object.values(instances).filter((i) => i.state === "running").length,
              total: Object.keys(instances).length,
            })}
          </p>
        ) : null}
      </header>
      <div className="px-3">
        <AsyncView
          height={height}
          loading={!away && !loaded && syncError === null}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(syncError === null ? {} : { error: syncError })}
          errorContext={STATUS_CONTEXT}
          onRetry={() => {
            runtime.refresh("services");
          }}
          empty={empty}
          emptyTitle={t("empty.title")}
          emptyDescription={t("empty.description")}
          emptyAction={
            <Button
              variant="primary"
              onClick={() => {
                void navigate({ to: CATALOG_PATH });
              }}
            >
              {t("empty.action")}
            </Button>
          }
        >
          <Groups groups={groups} selected={selected} now={now} onOpen={select} />
        </AsyncView>
      </div>
      <ServiceSheet
        instance={current}
        tool={current === undefined ? undefined : tools[current.tool]}
        now={now}
        onClose={() => {
          select(undefined);
        }}
      />
    </div>
  );
}
