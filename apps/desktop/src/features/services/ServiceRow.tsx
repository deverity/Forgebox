import { memo } from "react";
import { useTranslation } from "react-i18next";
import { restartInstance, startInstance, stopInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { FOCUS_RING } from "../../design/primitives/shared";
import { StatusDot } from "../../design/primitives/StatusDot";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { InstanceDto, PoolDto } from "../../ipc/generated";
import { countRender } from "../../qa/render-counter";
import { useInstanceMetrics, usePoolMetrics } from "../../state";
import { PHP_TOOL, dotState, isActive, mainPort, poolName, targetOf, uptimeMs } from "./model";
import { ToolIcon } from "./ToolIcon";

/**
 * Row grid shared by the header-less list: icon, name, state, ports, resources, actions. Every
 * column has a size that does not depend on the row's content (the actions column is fixed), so the
 * columns line up between rows and between groups (R-19).
 */
const GRID =
  "grid min-h-14 grid-cols-[24px_minmax(0,1.3fr)_minmax(0,1.1fr)_minmax(0,0.7fr)_minmax(0,0.9fr)_13rem] items-center gap-3 px-3";

const CELL = "min-w-0 truncate";

function useStateText(state: InstanceDto["state"], startedAt: string | null, now: number): string {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const label = t(`state.${state}`);
  const up = state === "running" ? uptimeMs(startedAt, now) : null;
  return up === null ? label : t("row.running", { state: label, uptime: format.duration(up) });
}

export interface InstanceRowProps {
  instance: InstanceDto;
  selected: boolean;
  now: number;
  onOpen: (id: string) => void;
}

/** One instance: icon, name and version, state with uptime, ports, CPU/RAM and the actions. */
export const InstanceRow = memo(function InstanceRow({
  instance,
  selected,
  now,
  onOpen,
}: InstanceRowProps) {
  if (import.meta.env.MODE === "qa") countRender("instance", instance.id);
  const { t } = useTranslation("services");
  const format = useFormatters();
  const samples = useInstanceMetrics(instance.id);
  const sample = samples[samples.length - 1];
  const cpu = sample?.cpu ?? instance.cpu_pct;
  const rss = sample?.rss ?? instance.rss_bytes;
  const target = targetOf(instance);
  const active = isActive(instance.state);
  const stateText = useStateText(instance.state, instance.started_at, now);
  const port = mainPort(instance);
  const ports = instance.ports.map((p) => p.port);
  return (
    <li
      data-service-row={instance.id}
      data-target={target}
      data-state={instance.state}
      aria-current={selected || undefined}
      className={`${GRID} rounded-control ${selected ? "bg-surface-2" : "hover:bg-surface-2"}`}
    >
      <ToolIcon tool={instance.tool} layoutId={`service-icon-${instance.id}`} />
      <button
        type="button"
        className={`${CELL} rounded-control py-2 text-left ${FOCUS_RING}`}
        onClick={() => {
          onOpen(instance.id);
        }}
      >
        <span className="block truncate text-body text-fg-primary">
          {instance.tool} {instance.version}
        </span>
        <span className="block truncate text-caption text-fg-secondary">{instance.name}</span>
      </button>
      <span className={`${CELL} flex items-center gap-2`} data-testid="row-state">
        <span aria-hidden="true">
          <StatusDot state={dotState(instance.state)} />
        </span>
        <span className="truncate text-callout text-fg-secondary">{stateText}</span>
      </span>
      <span className={`${CELL} text-callout text-fg-secondary ${TNUM_CLASS}`}>
        {port === null ? t("row.noPort") : ports.join(", ")}
      </span>
      <span className={`${CELL} text-callout text-fg-secondary ${TNUM_CLASS}`}>
        {cpu === null || rss === null
          ? t("row.noMetrics")
          : t("row.metrics", {
              cpu: format.number(cpu, { maximumFractionDigits: 1 }),
              ram: format.bytes(rss),
            })}
      </span>
      <span className="flex items-center justify-end gap-2">
        {active ? (
          <>
            <ActionButton
              action={restartInstance}
              input={target}
              loadingLabel={t("action.restarting")}
              successLabel={t("action.restarted")}
            >
              {t("action.restart")}
            </ActionButton>
            <ActionButton
              action={stopInstance}
              input={target}
              loadingLabel={t("action.stopping")}
              successLabel={t("action.stopped")}
            >
              {t("action.stop")}
            </ActionButton>
          </>
        ) : (
          <ActionButton
            variant="primary"
            action={startInstance}
            input={target}
            loadingLabel={t("action.starting")}
            successLabel={t("action.started")}
          >
            {t("action.start")}
          </ActionButton>
        )}
      </span>
    </li>
  );
});

export interface PoolRowProps {
  pool: PoolDto;
}

/** A PHP pool: busy/total workers, its FastCGI ports and RAM. Managed from the PHP screen. */
export const PoolRow = memo(function PoolRow({ pool }: PoolRowProps) {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const samples = usePoolMetrics(pool.version_id);
  const rss = samples[samples.length - 1]?.rss ?? pool.rss_bytes;
  return (
    <li
      data-service-row={pool.version_id}
      data-target={poolName(pool)}
      data-state={pool.state}
      className={`${GRID} rounded-control hover:bg-surface-2`}
    >
      <ToolIcon tool={PHP_TOOL} />
      <span className={CELL}>
        <span className="block truncate text-body text-fg-primary">
          {t("row.pool", { name: poolName(pool) })}
        </span>
        <span className="block truncate text-caption text-fg-secondary">{pool.version}</span>
      </span>
      <span className={`${CELL} flex items-center gap-2`} data-testid="row-state">
        <span aria-hidden="true">
          <StatusDot state={dotState(pool.state)} />
        </span>
        <span className="truncate text-callout text-fg-secondary">{t(`state.${pool.state}`)}</span>
      </span>
      <span className={`${CELL} text-callout text-fg-secondary ${TNUM_CLASS}`}>
        {pool.ports.join(", ")}
      </span>
      <span className={`${CELL} text-callout text-fg-secondary ${TNUM_CLASS}`}>
        {t("row.workers", { busy: pool.busy_workers, total: pool.workers })}
        {rss === null ? "" : ` · ${format.bytes(rss)}`}
      </span>
      <span />
    </li>
  );
});
