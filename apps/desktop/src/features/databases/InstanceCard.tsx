import { useTranslation } from "react-i18next";
import { startInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { StatusDot, type ServiceState } from "../../design/primitives/StatusDot";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type * as T from "../../ipc/generated";
import { monogram } from "./model";

const DOT: Record<T.ServiceState, ServiceState> = {
  stopped: "stopped",
  starting: "starting",
  running: "running",
  stopping: "starting",
  crashed: "failed",
  failed: "failed",
  maintenance: "updating",
};

/**
 * One database instance: monogram, name, version, state, port and the size of its data folder
 * ("≈ calculando…" until the daemon has measured it), with "Iniciar" while it is stopped.
 * The card is a radio of the group that picks the instance whose databases are listed.
 */
export function InstanceCard({
  instance,
  selected,
  onSelect,
}: {
  instance: T.DbInstanceDto;
  selected: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation("databases");
  const format = useFormatters();
  const stopped =
    instance.state === "stopped" || instance.state === "failed" || instance.state === "crashed";
  const size = instance.data_size_bytes;
  return (
    <div
      data-instance={instance.instance}
      data-state={instance.state}
      data-selected={selected}
      className={`flex w-72 shrink-0 flex-col gap-3 rounded-card border bg-surface p-4 ${
        selected ? "border-accent" : "border-stroke"
      }`}
    >
      <button
        type="button"
        role="radio"
        aria-checked={selected}
        aria-label={t("instance.select", { name: instance.instance })}
        onClick={onSelect}
        className="flex items-center gap-3 rounded-control text-left outline-none focus-visible:ring-2 focus-visible:ring-accent"
      >
        <span
          aria-hidden="true"
          className="flex size-10 items-center justify-center rounded-control border border-stroke bg-surface-2 font-semibold text-fg-secondary"
        >
          {monogram(instance)}
        </span>
        <span className="flex min-w-0 flex-col">
          <span className="truncate text-headline">{instance.instance}</span>
          <span className={`text-callout text-fg-secondary ${TNUM_CLASS}`}>
            {t("instance.version", { version: instance.version })}
          </span>
        </span>
      </button>
      <div className="flex flex-col gap-1 text-callout">
        <p className="flex items-center gap-2">
          <StatusDot state={DOT[instance.state]} />
          <span data-part="state">{t(`instance.state.${instance.state}`)}</span>
        </p>
        {instance.port === null ? null : (
          <p className={`text-fg-secondary ${TNUM_CLASS}`} data-part="port">
            {t("instance.port", { port: instance.port })}
          </p>
        )}
        <p className={`text-fg-secondary ${TNUM_CLASS}`} data-part="size">
          {size === null || size === undefined
            ? t("instance.sizePending")
            : t("instance.size", { size: format.bytes(size) })}
        </p>
      </div>
      {stopped ? (
        <ActionButton
          action={startInstance}
          input={instance.instance}
          variant="primary"
          loadingLabel={t("instance.starting")}
        >
          {t("instance.start")}
        </ActionButton>
      ) : null}
    </div>
  );
}
