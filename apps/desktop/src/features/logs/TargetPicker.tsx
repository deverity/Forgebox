import { useTranslation } from "react-i18next";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import type { LogTargetDto } from "../../ipc/generated";
import { useCapability, useConnectionStatus, useForgeQuery, useRuntime } from "../../state";
import { MAX_TARGETS } from "./model";

export const PICKER_SKELETON_HEIGHT = 240;
const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);

/** `logs.targets` grouped by `group`, in the order the daemon lists them. */
export function groupTargets(targets: readonly LogTargetDto[]): [string, LogTargetDto[]][] {
  const groups = new Map<string, LogTargetDto[]>();
  for (const target of targets) {
    const list = groups.get(target.group) ?? [];
    list.push(target);
    groups.set(target.group, list);
  }
  return [...groups.entries()];
}

export interface TargetPickerProps {
  selected: readonly string[];
  onChange: (selected: string[]) => void;
}

/** Source picker of the Logs screen: grouped checkboxes, at most 8 at once. */
export function TargetPicker({ selected, onChange }: TargetPickerProps) {
  const { t } = useTranslation("logs");
  const { t: tCommon } = useTranslation("common");
  const runtime = useRuntime();
  const status = useConnectionStatus();
  const capability = useCapability("logs.targets");
  const reason = useCapabilityReason(capability);
  const query = useForgeQuery("logs.targets", {});
  const full = selected.length >= MAX_TARGETS;

  // R-16: no daemon, or a daemon without `logs.targets`: say so, never an endless skeleton.
  if (AWAY.has(status)) {
    return (
      <EmptyState
        title={tCommon("ui.asyncDisconnectedTitle")}
        description={tCommon("ui.asyncDisconnectedHint")}
        action={
          <Button
            onClick={() => {
              void runtime.api.start();
            }}
          >
            {tCommon("ui.asyncRetry")}
          </Button>
        }
      />
    );
  }
  if (!capability.available && capability.reason !== "disconnected" && reason !== null) {
    return <EmptyState title={reason} />;
  }

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
    return (
      <div aria-busy="true" data-testid="targets-loading">
        <span className="sr-only">{t("targets.loading")}</span>
        <Skeleton height={PICKER_SKELETON_HEIGHT} />
      </div>
    );
  }
  const groups = groupTargets(query.data.targets);
  if (groups.length === 0) {
    return <p className="text-callout text-fg-secondary">{t("targets.empty")}</p>;
  }
  const toggle = (target: string, on: boolean) => {
    onChange(on ? [...selected, target] : selected.filter((id) => id !== target));
  };
  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <p className="text-callout text-fg-secondary" aria-live="polite">
          {t("targets.summary", { count: selected.length })}
        </p>
        {selected.length > 0 ? (
          <Button
            variant="ghost"
            onClick={() => {
              onChange([]);
            }}
          >
            {t("targets.clear")}
          </Button>
        ) : null}
      </div>
      {full ? (
        <p className="text-caption text-fg-tertiary">{t("targets.max", { max: MAX_TARGETS })}</p>
      ) : null}
      <div
        className="flex min-h-0 flex-col gap-3 overflow-y-auto"
        role="group"
        aria-label={t("targets.label")}
      >
        {groups.map(([group, items]) => (
          <fieldset key={group} className="flex flex-col gap-1" data-group={group}>
            <legend className="mb-1 text-caption font-semibold text-fg-secondary uppercase">
              {group === "daemon" || group === "php" ? t(`targets.groups.${group}`) : group}
            </legend>
            {items.map((target) => {
              const checked = selected.includes(target.target);
              return (
                <label
                  key={target.target}
                  className="flex min-h-7 items-center gap-2 text-body text-fg-primary"
                >
                  <input
                    type="checkbox"
                    className={cx("size-4 accent-(--accent)", FOCUS_RING)}
                    checked={checked}
                    disabled={!checked && full}
                    onChange={(event) => {
                      toggle(target.target, event.target.checked);
                    }}
                  />
                  <span className="truncate" title={target.target}>
                    {target.label}
                  </span>
                </label>
              );
            })}
          </fieldset>
        ))}
      </div>
    </div>
  );
}
