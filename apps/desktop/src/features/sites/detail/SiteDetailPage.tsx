import { Link, useParams } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { useStore } from "zustand";
import { FOCUS_RING } from "../../../design/primitives/shared";
import { openSite } from "../../../actions/sites";
import { SiteTabsHost } from "../../../app/hosts";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { Button } from "../../../design/primitives/Button";
import { EmptyState } from "../../../design/primitives/EmptyState";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { openUrl } from "../../../ipc/native";
import { useRuntime, useSite } from "../../../state";

const SITES_ROUTE = "/sites";

/** Route `/sites/:id`: header with the quick opens and the tabs from the site-tab slot registry. */
export function SiteDetailPage() {
  const { t } = useTranslation("sites");
  const runtime = useRuntime();
  const id = useParams({
    strict: false,
    select: (params: Record<string, unknown>) => (typeof params.id === "string" ? params.id : ""),
  });
  const site = useSite(id);
  const loaded = useStore(runtime.stores.sites, (state) => state.loaded);

  if (site === undefined) {
    return (
      <div data-testid="page-sites" className="flex min-h-0 flex-1 flex-col">
        <h1 className="px-4 pt-4 text-title-1">{t("title")}</h1>
        {loaded ? (
          <EmptyState
            title={t("detail.notFound.title")}
            description={t("detail.notFound.description")}
            action={
              <Link to={SITES_ROUTE}>
                <Button variant="secondary">{t("detail.notFound.back")}</Button>
              </Link>
            }
          />
        ) : (
          <div aria-busy="true" className="p-4">
            <span className="sr-only">{t("detail.loading")}</span>
            <Skeleton height={64} />
          </div>
        )}
      </div>
    );
  }

  return (
    <div data-testid="page-sites" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <Link
          to={SITES_ROUTE}
          className={`rounded-control text-callout text-fg-secondary hover:text-fg-primary ${FOCUS_RING}`}
        >
          {t("detail.header.back")}
        </Link>
        <h1 className="text-title-1">{site.host}</h1>
        <Button
          variant="ghost"
          title={t("detail.header.openUrl", { url: site.url })}
          onClick={() => {
            void openUrl(site.url);
          }}
        >
          {t("detail.header.openUrl", { url: site.url })}
        </Button>
        <div
          role="group"
          aria-label={t("detail.header.actions")}
          className="ml-auto flex items-center gap-2"
        >
          <ActionButton action={openSite} input={{ site: site.id, with: "explorer" }}>
            {t("detail.header.explorer")}
          </ActionButton>
          <ActionButton action={openSite} input={{ site: site.id, with: "editor" }}>
            {t("detail.header.editor")}
          </ActionButton>
          <ActionButton action={openSite} input={{ site: site.id, with: "terminal" }}>
            {t("detail.header.terminal")}
          </ActionButton>
        </div>
      </div>
      <SiteTabsHost siteId={site.id} />
    </div>
  );
}
