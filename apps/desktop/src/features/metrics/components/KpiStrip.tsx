import { useTranslation } from "react-i18next";
import { Skeleton } from "../../../design/primitives/Skeleton";
import type * as T from "../../../ipc/generated";
import { NO_VALUE, type MetricsFormatters } from "../format";

export const KPI_HEIGHT = 64;

export interface KpiStripProps {
  /** `stats.get` over 1 min (the rate "now"). */
  now: T.SiteStatsDto | null;
  /** `stats.get` over 5 min (percentiles, errors, resources, workers). */
  recent: T.SiteStatsDto | null;
  loading: boolean;
  formatters: MetricsFormatters;
  /** Site with a PHP pool; without it there are no workers. */
  php: boolean;
}

/** Header of docs/14 section 4.1: req/s, p50/p90/p99 (5 min), errors, CPU ≈, RAM ≈, workers. */
export function KpiStrip({ now, recent, loading, formatters: f, php }: KpiStripProps) {
  const { t } = useTranslation("metrics");
  const approx = recent?.approx ?? false;
  const cells: {
    id: "rps" | "p50" | "p90" | "p99" | "errors" | "cpu" | "ram" | "workers";
    value: string;
  }[] = [
    { id: "rps", value: f.rps(now?.rps) },
    { id: "p50", value: f.ms(recent?.p50_ms) },
    { id: "p90", value: f.ms(recent?.p90_ms) },
    { id: "p99", value: f.ms(recent?.p99_ms) },
    { id: "errors", value: f.percent(recent?.error_pct) },
    { id: "cpu", value: f.approx(f.percent(recent?.cpu_pct), approx) },
    { id: "ram", value: f.approx(f.bytes(recent?.mem_bytes), approx) },
    { id: "workers", value: php ? f.count(recent?.workers_active) : NO_VALUE },
  ];
  return (
    <dl
      data-testid="perf-kpis"
      aria-busy={loading}
      aria-label={t("site.kpi.group")}
      className="m-0 grid grid-cols-2 gap-2 sm:grid-cols-4 xl:grid-cols-8"
    >
      {cells.map((cell) => (
        <div
          key={cell.id}
          data-kpi={cell.id}
          style={{ height: KPI_HEIGHT }}
          className="flex min-w-0 flex-col justify-center gap-1 rounded-card border border-stroke bg-surface px-3"
        >
          <dt className="truncate text-caption text-fg-secondary">{t(`site.kpi.${cell.id}`)}</dt>
          <dd className={`m-0 truncate text-title-2 text-fg-primary ${f.tnum}`}>
            {loading ? (
              <>
                <span className="sr-only">{t("site.loading")}</span>
                <Skeleton height={22} width="60%" />
              </>
            ) : (
              cell.value
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}
