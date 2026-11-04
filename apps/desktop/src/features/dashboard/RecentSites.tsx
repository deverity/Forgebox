import { useNavigate } from "@tanstack/react-router";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "zustand";
import { openSite } from "../../actions/sites";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Badge } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { Skeleton } from "../../design/primitives/Skeleton";
import { FOCUS_RING } from "../../design/primitives/shared";
import { openUrl } from "../../ipc/native";
import { useConnectionStatus, useRuntime, useSites } from "../../state";
import { recentSites } from "./model";

const SITES_PATH = "/sites";

/**
 * Sites recentes: the 6 last updated. The URL opens on this machine; editor and terminal go
 * through `sites.open` (`local_only`, gated by the ActionButton like everywhere else).
 */
export function RecentSites({ height }: { height: number }) {
  const { t } = useTranslation("dashboard");
  const navigate = useNavigate();
  const sites = useSites();
  const status = useConnectionStatus();
  const loaded = useStore(useRuntime().stores.sites, (state) => state.loaded);
  const rows = useMemo(() => recentSites(Object.values(sites)), [sites]);
  const loading = !loaded && status !== "stopped" && status !== "closed";
  return (
    <section aria-labelledby="dashboard-recent" data-testid="dashboard-recent">
      <h2 id="dashboard-recent" className="pb-2 text-headline">
        {t("recent.title")}
      </h2>
      <div
        style={{ height }}
        aria-busy={loading}
        className="overflow-y-auto rounded-card border border-stroke bg-surface"
      >
        {loading ? (
          <div className="flex flex-col gap-2 p-3">
            {Array.from({ length: 5 }, (_, index) => (
              <Skeleton key={index} height={36} />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            title={t("recent.empty.title")}
            description={t("recent.empty.description")}
            action={
              <Button
                variant="primary"
                onClick={() => {
                  void navigate({ to: SITES_PATH });
                }}
              >
                {t("recent.empty.action")}
              </Button>
            }
          />
        ) : (
          <ul className="flex flex-col">
            {rows.map((site) => (
              <li
                key={site.id}
                data-recent-site={site.id}
                className="flex items-center gap-2 border-b border-stroke px-3 py-2 last:border-b-0"
              >
                <button
                  type="button"
                  title={t("recent.open", { host: site.host })}
                  aria-label={t("recent.open", { host: site.host })}
                  onClick={() => {
                    void openUrl(site.url);
                  }}
                  className={`min-w-0 flex-1 truncate rounded-control text-left text-body text-fg-primary hover:underline ${FOCUS_RING}`}
                >
                  {site.host}
                </button>
                {site.php === null ? null : (
                  <Badge>
                    {t("recent.php", { version: site.php.resolved ?? site.php.series })}
                  </Badge>
                )}
                <ActionButton action={openSite} input={{ site: site.id, with: "editor" }}>
                  {t("recent.editor")}
                </ActionButton>
                <ActionButton action={openSite} input={{ site: site.id, with: "terminal" }}>
                  {t("recent.terminal")}
                </ActionButton>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
