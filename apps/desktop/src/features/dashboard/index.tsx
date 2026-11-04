// The Dashboard (M2-E2-T14, docs/07 section 5.2): service tiles with sparklines, Atenção (from
// the source registry), Projetos (slot for E5), recent sites and activity.
import { useNavigate } from "@tanstack/react-router";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "zustand";
import { DashboardSectionsHost } from "../../app/hosts";
import { AsyncView } from "../../design/primitives/AsyncView";
import { Button } from "../../design/primitives/Button";
import { Skeleton } from "../../design/primitives/Skeleton";
import {
  useConnectionStatus,
  useInstances,
  useMetricsPoll,
  usePools,
  useRuntime,
  useSyncState,
} from "../../state";
import { ActivityFeed } from "./ActivityFeed";
import { AttentionSection } from "./AttentionSection";
import { TILE_HEIGHT, TILE_WIDTH, tilesOf } from "./model";
import { PROJECTS_SECTION, ProjectsSection } from "./ProjectsSection";
import { RecentSites } from "./RecentSites";
import { InstanceTile, PoolTile } from "./ServiceTiles";

/** Height of the tile strip: a tile plus room for the horizontal scrollbar. */
const STRIP_HEIGHT = TILE_HEIGHT + 16;
/** Height of the recent sites and activity cards (6 rows of 52 px). */
const CARD_HEIGHT = 312;
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const STATUS_CONTEXT = { method: "services.status", params: {} };
const SKELETON_TILES = 4;
const CATALOG_PATH = "/catalog";

function Tiles() {
  const { t } = useTranslation("dashboard");
  const runtime = useRuntime();
  const navigate = useNavigate();
  const status = useConnectionStatus();
  const instances = useInstances();
  const pools = usePools();
  const loaded = useStore(runtime.stores.services, (state) => state.loaded);
  const syncError = useSyncState((state) => state.errors.services ?? null);
  const tiles = useMemo(
    () => tilesOf(Object.values(instances), Object.values(pools)),
    [instances, pools],
  );
  const away = AWAY.has(status);
  const loading = !away && !loaded && syncError === null;
  return (
    <section aria-labelledby="dashboard-tiles" data-testid="dashboard-tiles">
      <h2 id="dashboard-tiles" className="sr-only">
        {t("tiles.label")}
      </h2>
      {loading ? (
        <div
          data-async-view="loading"
          aria-busy="true"
          style={{ height: STRIP_HEIGHT }}
          className="flex gap-3 overflow-hidden"
        >
          <span className="sr-only">{t("activity.loading")}</span>
          {Array.from({ length: SKELETON_TILES }, (_, index) => (
            <div key={index} className="shrink-0" style={{ width: TILE_WIDTH }}>
              <Skeleton height={TILE_HEIGHT} />
            </div>
          ))}
        </div>
      ) : (
        <AsyncView
          height={STRIP_HEIGHT}
          loading={false}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(syncError === null ? {} : { error: syncError })}
          errorContext={STATUS_CONTEXT}
          onRetry={() => {
            runtime.refresh("services");
          }}
          empty={tiles.length === 0}
          emptyTitle={t("tiles.empty.title")}
          emptyDescription={t("tiles.empty.description")}
          emptyAction={
            <Button
              variant="primary"
              onClick={() => {
                void navigate({ to: CATALOG_PATH });
              }}
            >
              {t("tiles.empty.action")}
            </Button>
          }
        >
          <ul className="flex gap-3" data-testid="tile-strip">
            {tiles.map((tile) =>
              tile.kind === "instance" ? (
                <InstanceTile key={tile.key} instance={tile.instance} />
              ) : (
                <PoolTile key={tile.key} pool={tile.pool} />
              ),
            )}
          </ul>
        </AsyncView>
      )}
    </section>
  );
}

/** Route `/dashboard`. Polls `services.status` only while mounted (the tiles' sparklines). */
export function DashboardPage() {
  const { t } = useTranslation("dashboard");
  useMetricsPoll();
  return (
    <div data-testid="page-dashboard" className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <header className="px-4 pt-4 pb-2">
        <h1 className="text-title-1">{t("title")}</h1>
      </header>
      <div className="flex flex-col gap-6 px-4 pb-6">
        <Tiles />
        <AttentionSection />
        <ProjectsSection />
        <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
          <RecentSites height={CARD_HEIGHT} />
          <ActivityFeed height={CARD_HEIGHT} />
        </div>
        <DashboardSectionsHost except={[PROJECTS_SECTION]} />
      </div>
    </div>
  );
}
