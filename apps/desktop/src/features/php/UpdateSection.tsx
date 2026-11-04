import { useState } from "react";
import { useTranslation } from "react-i18next";
import { checkToolUpdates, rollbackToolSeries } from "../../actions/tools";
import { ActionButton } from "../../design/primitives/ActionButton";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { ToolUpdatesDto, UpdatePolicy } from "../../ipc/generated";
import { useForgeQuery, useTools } from "../../state";
import { PolicyPicker } from "../services/tabs/VersionsTab";
import { rollbackCandidates, seriesPrefix } from "../services/tabs/versionsModel";
import { PHP_TOOL_ID } from "./model";

const DEFAULT_POLICY: UpdatePolicy = "notify";
const SEPARATOR = " · ";

/**
 * Updates of PHP: the policy of the tool (per-series policy is a later additive change, see the
 * note), "check now", and the history of this series with its rollback (docs/05 section 4).
 */
export function UpdateSection({ series }: { series: string }) {
  const { t } = useTranslation("php");
  const format = useFormatters();
  const policy = useTools((state) => state.tools[PHP_TOOL_ID]?.policy) ?? DEFAULT_POLICY;
  const history = useForgeQuery("tools.history", { tool: PHP_TOOL_ID, series, limit: 10 });
  const [found, setFound] = useState<ToolUpdatesDto | null>(null);
  const mine = found?.updates.filter((update) => seriesPrefix(update.from) === series) ?? [];
  const candidates = rollbackCandidates(history.data?.entries ?? []);
  return (
    <section aria-label={t("updates.title")} data-testid="updates" className="flex flex-col gap-3">
      <h3 className="text-headline">{t("updates.title")}</h3>
      <PolicyPicker tool={PHP_TOOL_ID} policy={policy} />
      <p className="text-caption text-fg-tertiary">{t("updates.policyNote")}</p>
      <div className="flex flex-col gap-2">
        <div>
          <ActionButton
            action={checkToolUpdates}
            input={PHP_TOOL_ID}
            loadingLabel={t("updates.checking")}
            successLabel={t("updates.checked")}
            onDone={(result) => {
              setFound(result as ToolUpdatesDto);
            }}
          >
            {t("updates.check")}
          </ActionButton>
        </div>
        {found === null ? null : (
          <p data-testid="update-result" className="text-callout text-fg-secondary">
            {mine.length === 0
              ? t("updates.upToDate")
              : mine
                  .map((update) => t("updates.available", { from: update.from, to: update.to }))
                  .join(SEPARATOR)}
          </p>
        )}
      </div>
      <div className="flex flex-col gap-2">
        <h4 className="text-callout font-semibold">{t("updates.history")}</h4>
        {history.isError ? (
          <ErrorNotice
            error={history.error}
            onRetry={() => {
              void history.refetch();
            }}
          />
        ) : history.data === undefined ? null : history.data.entries.length === 0 ? (
          <p className="text-callout text-fg-secondary">{t("updates.historyEmpty")}</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {history.data.entries.map((entry) => (
              <li
                key={entry.id}
                data-history={entry.id}
                className="flex items-center gap-2 text-callout"
              >
                <span className={`min-w-0 flex-1 ${TNUM_CLASS}`}>
                  {t("updates.historyLine", {
                    from: entry.from ?? "—",
                    to: entry.to,
                    status: t(`updates.status.${entry.status}`),
                  })}
                  <span className="block text-caption text-fg-tertiary">
                    {format.date(new Date(entry.started_at), {
                      dateStyle: "short",
                      timeStyle: "short",
                    })}
                  </span>
                </span>
                {candidates.get(series)?.id === entry.id ? (
                  <ActionButton
                    action={rollbackToolSeries}
                    input={`${PHP_TOOL_ID}@${series}`}
                    loadingLabel={t("updates.rollingBack")}
                    successLabel={t("updates.rolledBack")}
                  >
                    {t("updates.rollback")}
                  </ActionButton>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
