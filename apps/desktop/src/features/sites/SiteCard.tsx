import { Link } from "@tanstack/react-router";
import { memo } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { StatusDot, type ServiceState } from "../../design/primitives/StatusDot";
import type * as T from "../../ipc/generated";
import { openUrl } from "../../ipc/native";

const DETAIL_ROUTE = "/sites/$id";

export type SitesLayout = "grid" | "list";

/** Height of one card row in the grid and of one row in the list (px, fixed: no measuring). */
export const CARD_HEIGHT = 164;
export const ROW_HEIGHT = 56;

/** Two letters standing for the driver (thumbnails and favicons are deferred, docs/07 5.4). */
export function monogramOf(site: T.SiteDto): string {
  const source = site.kind === "proxy" ? "px" : (site.driver ?? "php");
  return source
    .replace(/[^a-z0-9]/gi, "")
    .slice(0, 2)
    .toUpperCase();
}

const DOT: Record<T.ServiceState, ServiceState> = {
  stopped: "stopped",
  starting: "starting",
  running: "running",
  stopping: "starting",
  crashed: "failed",
  failed: "failed",
  maintenance: "updating",
};

export interface SiteCardProps {
  site: T.SiteDto;
  layout: SitesLayout;
  onRemove: (site: T.SiteDto) => void;
}

/** One site as a card (grid) or a row (list). Pure over its props, memoized for fast typing. */
export const SiteCard = memo(function SiteCard({ site, layout, onRemove }: SiteCardProps) {
  const { t } = useTranslation("sites");
  const web = site.processes.find((process) => process.name === "web");
  const php = site.php?.resolved ?? site.php?.series ?? null;
  const monogram = (
    <span
      role="img"
      aria-label={t("card.monogram", { driver: site.driver ?? site.kind })}
      className="flex size-9 shrink-0 items-center justify-center rounded-control bg-surface-2 font-mono text-callout text-fg-secondary"
    >
      {monogramOf(site)}
    </span>
  );
  const host = (
    <button
      type="button"
      title={t("card.open", { host: site.host })}
      aria-label={t("card.open", { host: site.host })}
      onClick={() => {
        void openUrl(site.url);
      }}
      className="min-w-0 truncate rounded-control text-left text-headline text-fg-primary hover:underline focus-visible:outline-2 focus-visible:outline-accent"
    >
      {site.host}
    </button>
  );
  const badges = (
    <div className="flex flex-wrap items-center gap-1">
      {php !== null && site.kind !== "proxy" ? (
        <Badge>{t("badge.php", { version: php })}</Badge>
      ) : null}
      {site.server !== null ? <Badge>{site.server}</Badge> : null}
      <Badge tone={site.secure ? "success" : "neutral"}>
        {site.secure ? t("badge.https") : t("badge.http")}
      </Badge>
      {site.kind === "proxy" ? <Badge tone="info">{t("card.proxy")}</Badge> : null}
      {site.driver !== null ? <Badge>{site.driver}</Badge> : null}
      {site.websocket ? <Badge>{t("badge.ws")}</Badge> : null}
      <span
        title={
          site.case_sensitive ? t("badge.caseSensitiveTitle") : t("badge.caseInsensitiveTitle")
        }
      >
        <Badge>{site.case_sensitive ? t("badge.caseSensitive") : t("badge.caseInsensitive")}</Badge>
      </span>
    </div>
  );
  const process =
    web === undefined ? null : (
      <span className="flex items-center gap-1 text-caption text-fg-secondary">
        <StatusDot state={DOT[web.state]} />
        {t("card.web", { state: t(`process.${DOT[web.state]}`) })}
      </span>
    );
  const actions = (
    <div className="flex shrink-0 items-center gap-1">
      <Link
        to={DETAIL_ROUTE}
        params={{ id: site.id }}
        aria-label={t("card.details_aria", { host: site.host })}
        className="rounded-control px-2 py-1 text-callout text-fg-secondary hover:bg-surface-2 focus-visible:outline-2 focus-visible:outline-accent"
      >
        {t("card.details")}
      </Link>
      <Button
        variant="destructive"
        aria-label={t("card.remove_aria", { host: site.host })}
        onClick={() => {
          onRemove(site);
        }}
      >
        {t("card.remove")}
      </Button>
    </div>
  );

  if (layout === "list") {
    return (
      <div
        data-site-card={site.id}
        data-layout="list"
        className="flex items-center gap-3 rounded-control px-3 hover:bg-surface-2"
        style={{ height: ROW_HEIGHT }}
      >
        {monogram}
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <div className="flex items-center gap-2">
            {host}
            {process}
          </div>
          {badges}
        </div>
        {actions}
      </div>
    );
  }
  return (
    <div
      data-site-card={site.id}
      data-layout="grid"
      className="flex flex-col justify-between gap-2 overflow-hidden rounded-card border border-stroke bg-surface p-3"
      style={{ height: CARD_HEIGHT - 8 }}
    >
      <div className="flex items-center gap-3">
        {monogram}
        <div className="flex min-w-0 flex-1 flex-col">
          {host}
          {process}
        </div>
      </div>
      {badges}
      <div className="flex justify-end">{actions}</div>
    </div>
  );
});
