import { Link } from "@tanstack/react-router";
import { memo } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../../design/primitives/Badge";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { CatalogEntryDto } from "../../ipc/generated";
import { openUrl } from "../../ipc/native";
import { useForgeQuery } from "../../state";
import { CatalogIcon } from "./CatalogIcon";
import { useVisibleOnce } from "./hooks";
import { LINK_CLASS } from "./links";
import { latestOf, safeHttpUrl } from "./model";

const DETAIL_ROUTE = "/catalog/$id";

/** Card heights in px: fixed, so data arriving never moves the grid (docs/07 section 11). */
export const TOOL_CARD_HEIGHT = 184;
export const STACK_CARD_HEIGHT = 232;

export interface ToolCardProps {
  entry: CatalogEntryDto;
  /** Stacks are large cards. */
  large?: boolean;
}

/** Badges for the registry (official or third-party) and where the tool runs. */
export function EntryBadges({ entry }: { entry: CatalogEntryDto }) {
  const { t } = useTranslation("catalog");
  return (
    <>
      {entry.official === true ? (
        <Badge tone="success">{t("badge.official")}</Badge>
      ) : (
        <Badge tone="warning">{t("badge.thirdParty", { registry: entry.registry })}</Badge>
      )}
      {entry.native === false ? (
        <Badge tone="info">{t("badge.wsl")}</Badge>
      ) : (
        <Badge>{t("badge.native")}</Badge>
      )}
    </>
  );
}

/** The upstream changelog link; read from `tools.info` once the card is on screen. */
function ChangelogLink({ tool, enabled }: { tool: string; enabled: boolean }) {
  const { t } = useTranslation("catalog");
  const info = useForgeQuery("tools.info", { tool }, { enabled });
  const url = safeHttpUrl(info.data?.changelog_url);
  if (url === null) return <span className="h-5" aria-hidden="true" />;
  return (
    <a
      href={url}
      rel="noopener noreferrer"
      data-testid="changelog-link"
      className={`text-caption ${LINK_CLASS}`}
      onClick={(event) => {
        event.preventDefault();
        void openUrl(url);
      }}
    >
      {t("card.changelog")}
    </a>
  );
}

/** One catalog entry as a card (large for stacks). Pure over its props, memoized. */
export const ToolCard = memo(function ToolCard({ entry, large = false }: ToolCardProps) {
  const { t } = useTranslation("catalog");
  const format = useFormatters();
  const [ref, visible] = useVisibleOnce<HTMLDivElement>();
  const latest = latestOf(entry);
  const summary = entry.summary ?? entry.description ?? "";
  const size = entry.size_estimate_bytes;
  const update = entry.update_available ?? null;
  return (
    <div
      ref={ref}
      data-catalog-card={entry.id}
      data-kind={entry.kind ?? "tool"}
      style={{ height: large ? STACK_CARD_HEIGHT : TOOL_CARD_HEIGHT }}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-4"
    >
      <div className="flex items-center gap-3">
        <CatalogIcon tool={entry.id} sha256={entry.icon_sha256} size={large ? 48 : 32} />
        <div className="min-w-0 flex-1">
          <Link
            to={DETAIL_ROUTE}
            params={{ id: entry.id }}
            className={`block truncate ${large ? "text-title-2" : "text-headline"} outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-accent hover:underline`}
          >
            {entry.name}
          </Link>
          <span className="block truncate text-caption text-fg-tertiary">
            {entry.category ?? ""}
          </span>
        </div>
        {entry.installed ? <Badge tone="accent">{t("badge.installed")}</Badge> : null}
      </div>
      <p className={`text-callout text-fg-secondary ${large ? "line-clamp-4" : "line-clamp-2"}`}>
        {summary}
      </p>
      <div className="mt-auto flex flex-wrap items-center gap-1.5">
        <EntryBadges entry={entry} />
        {update === null ? null : (
          <Badge tone="info">{t("badge.update", { version: update })}</Badge>
        )}
      </div>
      <div className={`flex items-center gap-3 text-caption text-fg-secondary ${TNUM_CLASS}`}>
        <span>{latest === null ? t("card.noVersion") : t("card.latest", { version: latest })}</span>
        {size === null || size === undefined ? null : (
          <span>{t("card.size", { size: format.bytes(size) })}</span>
        )}
        <span className="flex-1" />
        <ChangelogLink tool={entry.id} enabled={visible} />
      </div>
    </div>
  );
});
