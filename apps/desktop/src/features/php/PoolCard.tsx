import { useTranslation } from "react-i18next";
import { StatusDot } from "../../design/primitives/StatusDot";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { PhpVersionDto } from "../../ipc/generated";
import { usePool, usePoolMetrics } from "../../state";
import { dotState } from "../services/model";
import { latestRate, livePool } from "./model";

const NO_VALUE = "—";

/**
 * Pool card of a series: state, busy/total workers, req/s (the `requests_total` delta the
 * metrics poll computes every 2 s) and RSS. The live store entry wins over the `php.list` one.
 */
export function PoolCard({ version }: { version: PhpVersionDto }) {
  const { t } = useTranslation("php");
  const format = useFormatters();
  const pool = livePool(version.pool, usePool(version.version_id));
  const samples = usePoolMetrics(version.version_id);
  if (pool === null) {
    return (
      <p data-testid="pool-card" data-state="none" className="text-callout text-fg-secondary">
        {t("pool.none")}
      </p>
    );
  }
  const rate = latestRate(samples);
  const rss = samples[samples.length - 1]?.rss ?? pool.rss_bytes;
  return (
    <div
      data-testid="pool-card"
      data-state={pool.state}
      className="flex flex-col gap-1 rounded-control border border-stroke bg-surface-2 p-3"
    >
      <div className="flex items-center gap-2">
        <span aria-hidden="true">
          <StatusDot state={dotState(pool.state)} />
        </span>
        <span className="text-body">{t(`pool.state.${pool.state}`)}</span>
      </div>
      <div
        className={`grid grid-cols-2 gap-x-3 gap-y-1 text-callout text-fg-secondary ${TNUM_CLASS}`}
      >
        <span data-testid="pool-workers">
          {t("pool.workers", { busy: pool.busy_workers, total: pool.workers })}
        </span>
        <span data-testid="pool-rate" data-value={rate ?? undefined}>
          {rate === null
            ? t("pool.noRate")
            : t("pool.rate", {
                rate: format.number(rate, { minimumFractionDigits: 1, maximumFractionDigits: 1 }),
              })}
        </span>
        <span data-testid="pool-rss" className="col-span-2">
          {rss === null ? NO_VALUE : t("pool.rss", { size: format.bytes(rss) })}
        </span>
      </div>
    </div>
  );
}
