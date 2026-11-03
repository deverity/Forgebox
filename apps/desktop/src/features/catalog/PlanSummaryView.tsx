import type { UseQueryResult } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { ForgeApiError } from "../../ipc/errors";
import type { ToolPlanDto } from "../../ipc/generated";
import { summarizePlan } from "./model";

export interface PlanSummaryViewProps {
  query: UseQueryResult<ToolPlanDto, ForgeApiError>;
  /** The query is not running (nothing chosen yet, or the daemon cannot plan). */
  idle?: boolean;
  /** Reserved height in px, so the controls below do not move when the plan arrives. */
  minHeight?: number;
}

/**
 * What an install will download (`tools.plan`): each item with its size, the total (with the
 * approximate sign when any size is a manifest estimate) and what is already installed. Shown
 * BEFORE the user confirms (docs/12, docs/07 section 5.9).
 */
export function PlanSummaryView({ query, idle = false, minHeight }: PlanSummaryViewProps) {
  const { t } = useTranslation("catalog");
  const format = useFormatters();
  if (query.isError) {
    return (
      <ErrorNotice
        error={query.error}
        onRetry={() => {
          void query.refetch();
        }}
      />
    );
  }
  if (query.data === undefined) {
    if (idle) return null;
    return (
      <div aria-busy="true" data-testid="plan-loading">
        <span className="sr-only">{t("plan.loading")}</span>
        <Skeleton height={minHeight ?? 72} />
      </div>
    );
  }
  const summary = summarizePlan(query.data);
  const total = format.bytes(summary.totalBytes);
  return (
    <section
      aria-label={t("plan.title")}
      data-testid="plan"
      data-plan-total={summary.totalBytes}
      style={minHeight === undefined ? undefined : { minHeight }}
      className="flex flex-col gap-2 rounded-card border border-stroke p-3"
    >
      <h3 className="text-headline">{t("plan.title")}</h3>
      <ul className="flex flex-col gap-1">
        {summary.download.map((item) => (
          <li key={item.spec} className={`flex items-center gap-2 text-callout ${TNUM_CLASS}`}>
            <span className="min-w-0 flex-1 truncate">
              {t("plan.item", { tool: item.tool, version: item.version })}
            </span>
            <span className="text-fg-secondary">
              {item.size_bytes === null || item.size_bytes === undefined
                ? t("plan.sizeUnknown")
                : `${item.size_estimated === true ? t("plan.approx") : ""}${format.bytes(item.size_bytes)}`}
            </span>
          </li>
        ))}
        {summary.present.map((item) => (
          <li key={item.spec} className="flex items-center gap-2 text-callout text-fg-tertiary">
            <span className="min-w-0 flex-1 truncate">
              {t("plan.item", { tool: item.tool, version: item.version })}
            </span>
            <span>{t("plan.alreadyInstalled")}</span>
          </li>
        ))}
      </ul>
      <p className={`text-body ${TNUM_CLASS}`} data-testid="plan-total">
        {summary.download.length === 0
          ? t("plan.nothing")
          : t("plan.total", { size: `${summary.approximate ? t("plan.approx") : ""}${total}` })}
      </p>
      {summary.unknownSize > 0 ? (
        <p className="text-caption text-fg-tertiary">
          {t("plan.unknownSize", { count: summary.unknownSize })}
        </p>
      ) : null}
    </section>
  );
}
