import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../../../design/primitives/Badge";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { useFormatters } from "../../../design/primitives/useFormatters";
import type * as T from "../../../ipc/generated";
import type { MetricsFormatters } from "../format";
import { TABLE_ROWS, slowestRoutes } from "./model";

const ROW_HEIGHT = 32;
/** Header + rows: the same box for skeleton, empty and data (no layout shift). */
export const TABLE_HEIGHT = ROW_HEIGHT * (TABLE_ROWS + 1);
const TIME_FORMAT: Intl.DateTimeFormatOptions = {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
};
const CELL = "truncate px-3 text-body";
const HEAD = "text-left text-caption text-fg-secondary";

function Frame({
  id,
  title,
  loading,
  children,
}: {
  id: string;
  title: string;
  loading: boolean;
  children: ReactNode;
}) {
  const { t } = useTranslation("metrics");
  return (
    <section
      aria-labelledby={`${id}-title`}
      data-testid={id}
      aria-busy={loading}
      className="flex min-w-0 flex-col gap-2"
    >
      <h3 id={`${id}-title`} className="text-headline text-fg-primary">
        {title}
      </h3>
      <div
        style={{ height: TABLE_HEIGHT }}
        className="overflow-hidden rounded-card border border-stroke bg-surface"
      >
        {loading ? (
          <>
            <span className="sr-only">{t("site.loading")}</span>
            <Skeleton height={TABLE_HEIGHT} />
          </>
        ) : (
          children
        )}
      </div>
    </section>
  );
}

function Message({ text }: { text: string }) {
  return (
    <p
      style={{ height: TABLE_HEIGHT }}
      className="m-0 flex items-center justify-center px-4 text-center text-callout text-fg-secondary"
    >
      {text}
    </p>
  );
}

/** "Rotas mais lentas" and "Erros recentes" from `stats.routes` (access log, v1). */
export function RoutesTables({
  data,
  loading,
  failed,
  formatters: f,
}: {
  data: T.RoutesDto | undefined;
  loading: boolean;
  failed: boolean;
  formatters: MetricsFormatters;
}) {
  const { t } = useTranslation("metrics");
  const format = useFormatters();
  const routes = data === undefined ? [] : slowestRoutes(data.routes);
  const errors = data?.recent_errors.slice(0, TABLE_ROWS) ?? [];
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Frame id="perf-routes" title={t("routes.title")} loading={loading}>
        {failed ? (
          <Message text={t("site.routes.failed")} />
        ) : routes.length === 0 ? (
          <Message text={t("routes.empty")} />
        ) : (
          <table className="w-full table-fixed border-collapse">
            <thead>
              <tr style={{ height: ROW_HEIGHT }} className={HEAD}>
                <th className="px-3 font-normal">{t("site.routes.route")}</th>
                <th className="w-16 px-3 text-right font-normal">{t("site.routes.p50")}</th>
                <th className="w-16 px-3 text-right font-normal">{t("site.routes.p99")}</th>
                <th className="w-16 px-3 text-right font-normal">{t("site.routes.requests")}</th>
              </tr>
            </thead>
            <tbody>
              {routes.map((route) => (
                <tr
                  key={`${route.method} ${route.route}`}
                  data-route={route.route}
                  style={{ height: ROW_HEIGHT }}
                  className="border-t border-stroke"
                >
                  <td
                    className={`${CELL} font-mono text-mono`}
                    title={`${route.method} ${route.route}`}
                  >
                    <span className="text-fg-tertiary">{route.method}</span> {route.route}
                  </td>
                  <td className={`${CELL} text-right ${f.tnum}`}>{f.ms(route.p50_ms)}</td>
                  <td className={`${CELL} text-right ${f.tnum}`}>{f.ms(route.p99_ms)}</td>
                  <td className={`${CELL} text-right ${f.tnum}`}>{f.count(route.count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Frame>
      <Frame id="perf-errors" title={t("routes.errors")} loading={loading}>
        {failed ? (
          <Message text={t("site.routes.failed")} />
        ) : errors.length === 0 ? (
          <Message text={t("site.routes.noErrors")} />
        ) : (
          <table className="w-full table-fixed border-collapse">
            <thead>
              <tr style={{ height: ROW_HEIGHT }} className={HEAD}>
                <th className="w-24 px-3 font-normal">{t("site.routes.time")}</th>
                <th className="w-16 px-3 font-normal">{t("site.routes.status")}</th>
                <th className="px-3 font-normal">{t("site.routes.route")}</th>
                <th className="w-16 px-3 text-right font-normal">{t("site.routes.duration")}</th>
              </tr>
            </thead>
            <tbody>
              {errors.map((error, index) => (
                <tr
                  key={`${error.at}-${String(index)}`}
                  data-status={error.status}
                  style={{ height: ROW_HEIGHT }}
                  className="border-t border-stroke"
                >
                  <td className={`${CELL} ${f.tnum}`}>
                    {format.date(Date.parse(error.at), TIME_FORMAT)}
                  </td>
                  <td className="px-3">
                    <Badge tone={error.status >= 500 ? "danger" : "warning"}>
                      {String(error.status)}
                    </Badge>
                  </td>
                  <td
                    className={`${CELL} font-mono text-mono`}
                    title={`${error.method} ${error.route}`}
                  >
                    <span className="text-fg-tertiary">{error.method}</span> {error.route}
                  </td>
                  <td className={`${CELL} text-right ${f.tnum}`}>{f.ms(error.duration_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Frame>
    </div>
  );
}
