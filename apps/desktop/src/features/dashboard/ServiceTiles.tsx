import { useNavigate } from "@tanstack/react-router";
import { memo, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { restartInstance, startInstance, stopInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Button } from "../../design/primitives/Button";
import { Popover } from "../../design/primitives/Popover";
import { Sparkline } from "../../design/primitives/Sparkline";
import { StatusDot } from "../../design/primitives/StatusDot";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { InstanceDto, PoolDto } from "../../ipc/generated";
import { useInstanceMetrics, usePoolMetrics } from "../../state";
import { PHP_TOOL, dotState, isActive, mainPort, poolName, targetOf } from "../services/model";
import { ToolIcon } from "../services/ToolIcon";
import { TILE_HEIGHT, TILE_WIDTH, instanceSeries, poolSeries } from "./model";

const ELLIPSIS = "…";
const LOGS_PATH = "/logs";

const TILE_CLASS =
  "flex shrink-0 flex-col justify-between gap-2 rounded-card border border-stroke bg-surface p-3";

/** The `…` menu: restart (instances), logs and details. A popover with plain buttons. */
function TileMenu({
  name,
  restartTarget,
  detail,
}: {
  name: string;
  restartTarget: string | null;
  detail: { to: "/services"; search: { instance: string } } | { to: "/php" };
}) {
  const { t } = useTranslation("dashboard");
  const navigate = useNavigate();
  return (
    <Popover
      label={t("tiles.menu", { name })}
      align="end"
      trigger={
        <Button
          variant="ghost"
          aria-label={t("tiles.menu", { name })}
          title={t("tiles.menu", { name })}
        >
          <span aria-hidden="true">{ELLIPSIS}</span>
        </Button>
      }
    >
      <div className="flex min-w-40 flex-col gap-1" data-testid="tile-menu">
        <Button
          variant="ghost"
          onClick={() => {
            void navigate({ to: LOGS_PATH });
          }}
        >
          {t("tiles.logs")}
        </Button>
        <Button
          variant="ghost"
          onClick={() => {
            void navigate(detail);
          }}
        >
          {t("tiles.detail")}
        </Button>
        {/* Last: the first focus lands on a plain button, so Escape closes the popover at once
            (an ActionButton's tooltip would take the first Escape). */}
        {restartTarget === null ? null : (
          <ActionButton
            variant="ghost"
            action={restartInstance}
            input={restartTarget}
            loadingLabel={t("tiles.restarting")}
            successLabel={t("tiles.restarted")}
          >
            {t("tiles.restart")}
          </ActionButton>
        )}
      </div>
    </Popover>
  );
}

function Charts({
  cpu,
  ram,
  cpuLabel,
  ramLabel,
}: {
  cpu: readonly number[];
  ram: readonly number[];
  cpuLabel: string;
  ramLabel: string;
}) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <div className="min-w-0">
        <p className={`truncate text-caption text-fg-secondary ${TNUM_CLASS}`}>{cpuLabel}</p>
        <Sparkline samples={cpu} width={96} height={24} showLabels={false} />
      </div>
      <div className="min-w-0">
        <p className={`truncate text-caption text-fg-secondary ${TNUM_CLASS}`}>{ramLabel}</p>
        <Sparkline samples={ram} width={96} height={24} showLabels={false} />
      </div>
    </div>
  );
}

export const InstanceTile = memo(function InstanceTile({ instance }: { instance: InstanceDto }) {
  const { t } = useTranslation("dashboard");
  const { t: ts } = useTranslation("services");
  const format = useFormatters();
  const samples = useInstanceMetrics(instance.id);
  const series = useMemo(() => instanceSeries(samples), [samples]);
  const last = samples[samples.length - 1];
  const cpu = last?.cpu ?? instance.cpu_pct;
  const rss = last?.rss ?? instance.rss_bytes;
  const target = targetOf(instance);
  const port = mainPort(instance);
  return (
    <li
      data-tile={instance.id}
      data-target={target}
      data-state={instance.state}
      style={{ height: TILE_HEIGHT, width: TILE_WIDTH }}
      className={TILE_CLASS}
    >
      <div className="flex items-center gap-2">
        <ToolIcon tool={instance.tool} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-body text-fg-primary">
            {instance.tool} {instance.version}
          </p>
          <p className="truncate text-caption text-fg-secondary">{instance.name}</p>
        </div>
        <TileMenu
          name={target}
          restartTarget={isActive(instance.state) ? target : null}
          detail={{ to: "/services", search: { instance: instance.id } }}
        />
      </div>
      <div className="flex items-center gap-2">
        <span aria-hidden="true">
          <StatusDot state={dotState(instance.state)} />
        </span>
        <span className="min-w-0 flex-1 truncate text-callout text-fg-secondary">
          {ts(`state.${instance.state}`)}
          {port === null ? "" : ` · :${String(port)}`}
        </span>
        {isActive(instance.state) ? (
          <ActionButton
            action={stopInstance}
            input={target}
            loadingLabel={t("tiles.stopping")}
            successLabel={t("tiles.stopped")}
          >
            {t("tiles.stop")}
          </ActionButton>
        ) : (
          <ActionButton
            variant="primary"
            action={startInstance}
            input={target}
            loadingLabel={t("tiles.starting")}
            successLabel={t("tiles.started")}
          >
            {t("tiles.start")}
          </ActionButton>
        )}
      </div>
      <Charts
        cpu={series.cpu}
        ram={series.ram}
        cpuLabel={
          cpu === null
            ? t("tiles.noMetrics")
            : t("tiles.cpu", { value: `${format.number(cpu, { maximumFractionDigits: 1 })}%` })
        }
        ramLabel={
          rss === null ? t("tiles.noMetrics") : t("tiles.ram", { value: format.bytes(rss) })
        }
      />
    </li>
  );
});

export const PoolTile = memo(function PoolTile({ pool }: { pool: PoolDto }) {
  const { t } = useTranslation("dashboard");
  const { t: ts } = useTranslation("services");
  const format = useFormatters();
  const samples = usePoolMetrics(pool.version_id);
  const series = useMemo(() => poolSeries(samples), [samples]);
  const last = samples[samples.length - 1];
  const rss = last?.rss ?? pool.rss_bytes;
  const rps = last?.rps ?? null;
  return (
    <li
      data-tile={pool.version_id}
      data-target={poolName(pool)}
      data-state={pool.state}
      style={{ height: TILE_HEIGHT, width: TILE_WIDTH }}
      className={TILE_CLASS}
    >
      <div className="flex items-center gap-2">
        <ToolIcon tool={PHP_TOOL} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-body text-fg-primary">
            {t("tiles.pool", { series: pool.series })}
          </p>
          <p className="truncate text-caption text-fg-secondary">{pool.version}</p>
        </div>
        <TileMenu name={poolName(pool)} restartTarget={null} detail={{ to: "/php" }} />
      </div>
      <div className="flex items-center gap-2">
        <span aria-hidden="true">
          <StatusDot state={dotState(pool.state)} />
        </span>
        <span className="min-w-0 flex-1 truncate text-callout text-fg-secondary">
          {ts(`state.${pool.state}`)}
          {pool.ports[0] === undefined ? "" : ` · :${String(pool.ports[0])}`}
        </span>
        <span className={`text-caption text-fg-secondary ${TNUM_CLASS}`}>
          {t("tiles.workers", { busy: pool.busy_workers, total: pool.workers })}
        </span>
      </div>
      <Charts
        cpu={series.cpu}
        ram={series.ram}
        cpuLabel={
          rps === null
            ? t("tiles.noMetrics")
            : t("tiles.reqs", { value: format.number(rps, { maximumFractionDigits: 1 }) })
        }
        ramLabel={
          rss === null ? t("tiles.noMetrics") : t("tiles.ram", { value: format.bytes(rss) })
        }
      />
    </li>
  );
});
