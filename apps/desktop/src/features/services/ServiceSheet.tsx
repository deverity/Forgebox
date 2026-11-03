import { Tabs } from "radix-ui";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { restartInstance, startInstance, stopInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Sheet } from "../../design/primitives/Sheet";
import { StatusDot } from "../../design/primitives/StatusDot";
import { useFormatters } from "../../design/primitives/useFormatters";
import type { InstanceDto, ToolDto } from "../../ipc/generated";
import { InstancesTab } from "./InstancesTab";
import { LogsTab } from "./LogsTab";
import { dotState, isActive, targetOf, uptimeMs } from "./model";
import { OverviewTab } from "./OverviewTab";
import { ToolIcon } from "./ToolIcon";

export const SERVICE_TABS = ["overview", "logs", "instances"] as const;
export type ServiceTab = (typeof SERVICE_TABS)[number];

const TAB_CLASS =
  "-mb-px border-b-2 border-transparent px-3 py-2 text-callout text-fg-secondary outline-hidden hover:text-fg-primary focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent data-[state=active]:border-accent data-[state=active]:text-fg-primary";

export interface ServiceSheetProps {
  instance: InstanceDto | undefined;
  tool: ToolDto | undefined;
  now: number;
  onClose: () => void;
}

/** The 420 px detail sheet of an instance: Visao geral, Logs and Instancias. */
export function ServiceSheet({ instance, tool, now, onClose }: ServiceSheetProps) {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const [tab, setTab] = useState<ServiceTab>(SERVICE_TABS[0]);
  const open = instance !== undefined;
  const target = instance === undefined ? "" : targetOf(instance);
  const up = instance?.state === "running" ? uptimeMs(instance.started_at, now) : null;
  return (
    <Sheet
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setTab(SERVICE_TABS[0]);
          onClose();
        }
      }}
      title={target === "" ? t("sheet.title") : `${tool?.name ?? instance?.tool ?? ""} · ${target}`}
      description={t("sheet.description")}
    >
      {instance === undefined ? null : (
        <div className="flex flex-col gap-4" data-testid="service-sheet" data-target={target}>
          <div className="flex items-center gap-3">
            <ToolIcon tool={instance.tool} layoutId={`service-icon-${instance.id}`} />
            <span aria-hidden="true">
              <StatusDot state={dotState(instance.state)} />
            </span>
            <span className="min-w-0 flex-1 truncate text-callout text-fg-secondary">
              {up === null
                ? t(`state.${instance.state}`)
                : t("row.running", {
                    state: t(`state.${instance.state}`),
                    uptime: format.duration(up),
                  })}
            </span>
            {isActive(instance.state) ? (
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
          </div>
          <Tabs.Root
            value={tab}
            onValueChange={(value) => {
              setTab(value as ServiceTab);
            }}
          >
            <Tabs.List aria-label={t("sheet.tabs")} className="flex border-b border-stroke">
              {SERVICE_TABS.map((id) => (
                <Tabs.Trigger key={id} value={id} className={TAB_CLASS}>
                  {t(`tabs.${id}`)}
                </Tabs.Trigger>
              ))}
            </Tabs.List>
            <Tabs.Content value={SERVICE_TABS[0]} className="pt-4 outline-hidden">
              <OverviewTab
                instance={instance}
                onShowLogs={() => {
                  setTab(SERVICE_TABS[1]);
                }}
              />
            </Tabs.Content>
            <Tabs.Content value={SERVICE_TABS[1]} className="pt-4 outline-hidden">
              <LogsTab key={target} target={target} />
            </Tabs.Content>
            <Tabs.Content value={SERVICE_TABS[2]} className="pt-4 outline-hidden">
              <InstancesTab
                instance={instance}
                tool={tool}
                onRemoved={(id) => {
                  if (id === instance.id) onClose();
                }}
              />
            </Tabs.Content>
          </Tabs.Root>
        </div>
      )}
    </Sheet>
  );
}
