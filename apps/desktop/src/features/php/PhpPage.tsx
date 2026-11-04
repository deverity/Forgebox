import { useNavigate } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { AsyncView } from "../../design/primitives/AsyncView";
import { FOCUS_RING } from "../../design/primitives/shared";
import { useConnectionStatus, useForgeQuery, useRuntime } from "../../state";
import { seriesColumns } from "./model";
import { SeriesColumn } from "./SeriesColumn";

const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const LIST_CONTEXT = { method: "php.list", params: {} };
const CATALOG_PHP_PATH = "/catalog/php";
/** Height AsyncView reserves while loading or empty; the columns fill the rest on their own. */
const STATE_HEIGHT = 360;

/**
 * Route `/php` (docs/07 section 5.5): one column per installed series, side by side and
 * scrolling horizontally when they do not fit (960 px shows about two and a half).
 */
export function PhpPage() {
  const { t } = useTranslation("php");
  const runtime = useRuntime();
  const navigate = useNavigate();
  const status = useConnectionStatus();
  const list = useForgeQuery("php.list", {});
  const columns = seriesColumns(list.data?.versions ?? []);
  const away = AWAY.has(status);
  const installOther = () => {
    void navigate({ to: CATALOG_PHP_PATH });
  };
  return (
    <div data-testid="page-php" className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex items-baseline justify-between gap-3 px-4 pt-4 pb-3">
        <h1 className="text-title-1">{t("title")}</h1>
        <div className="flex items-center gap-3">
          {columns.length === 0 ? null : (
            <p className="text-callout text-fg-secondary">
              {t("summary", { count: columns.length })}
            </p>
          )}
          <Button variant="secondary" onClick={installOther}>
            {t("install")}
          </Button>
        </div>
      </header>
      {columns.length > 0 ? (
        <div
          data-testid="php-columns"
          role="region"
          aria-label={t("columns")}
          // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable region must be keyboard reachable
          tabIndex={0}
          className={`flex min-h-0 flex-1 gap-4 overflow-x-auto px-4 pb-4 ${FOCUS_RING}`}
        >
          {columns.map((version) => (
            <SeriesColumn key={version.series} version={version} />
          ))}
        </div>
      ) : (
        <div className="px-3">
          <AsyncView
            height={STATE_HEIGHT}
            loading={!away && list.isPending}
            disconnected={away}
            onStartDaemon={() => {
              void runtime.api.start();
            }}
            {...(list.isError ? { error: list.error } : {})}
            errorContext={LIST_CONTEXT}
            onRetry={() => {
              void list.refetch();
            }}
            empty
            emptyTitle={t("empty.title")}
            emptyDescription={t("empty.description")}
            emptyAction={
              <Button variant="primary" onClick={installOther}>
                {t("empty.action")}
              </Button>
            }
          >
            <span />
          </AsyncView>
        </div>
      )}
    </div>
  );
}
