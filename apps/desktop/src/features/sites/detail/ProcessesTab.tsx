import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { projectDown, projectUp, runSiteProcess } from "../../../actions/sites";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { Button } from "../../../design/primitives/Button";
import { EmptyState } from "../../../design/primitives/EmptyState";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { StatusDot, type ServiceState } from "../../../design/primitives/StatusDot";
import type * as T from "../../../ipc/generated";
import { useForgeQuery, useSite } from "../../../state";
import { LogViewer } from "../../logs/LogViewer";
import { isServicePayload, processOfEvent } from "./model";
import { useSiteEvents } from "./useSiteEvents";

const DOT: Record<T.ServiceState, ServiceState> = {
  stopped: "stopped",
  starting: "starting",
  running: "running",
  stopping: "starting",
  crashed: "failed",
  failed: "failed",
  maintenance: "updating",
};

const NO_LIVE: Record<string, never> = {};
const SEPARATOR = " · ";

type Live = Partial<Pick<T.SiteProcessDto, "state" | "pid" | "instance_id">>;

/**
 * "Processos": the Procfile processes from `sites.ps`, with start/stop/restart (`sites.run`).
 * Rows follow the `service.*` events that carry this `site_id`; an event that matches no row
 * triggers a `sites.ps` refetch (the state layer also invalidates it on every service event).
 */
export function ProcessesTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("sites");
  const site = useSite(siteId);
  const query = useForgeQuery("sites.ps", { site: siteId });
  const [openLogs, setOpenLogs] = useState<string | null>(null);
  const [patched, setPatched] = useState<{
    base: readonly T.SiteProcessDto[] | undefined;
    rows: Record<string, Live>;
  }>({ base: undefined, rows: {} });
  const processes = query.data?.processes;
  const { refetch } = query;

  // A `sites.ps` that differs from the one the events patched is the truth: the patch is dropped.
  const live = patched.base === processes ? patched.rows : NO_LIVE;

  useSiteEvents(siteId, (event) => {
    if (!event.kind.startsWith("service.") || !isServicePayload(event.payload)) return;
    const payload = event.payload;
    const row = processOfEvent(processes ?? [], payload);
    if (row === undefined) {
      void refetch();
      return;
    }
    setPatched((current) => ({
      base: processes,
      rows: {
        ...(current.base === processes ? current.rows : NO_LIVE),
        [row.name]: { state: payload.state, pid: payload.pid, instance_id: payload.instance_id },
      },
    }));
  });

  const done = useCallback(() => {
    void refetch();
  }, [refetch]);

  if (query.isError) {
    return (
      <ErrorNotice
        error={query.error}
        onRetry={() => {
          void refetch();
        }}
      />
    );
  }
  if (processes === undefined) {
    return (
      <div aria-busy="true" data-testid="processes-loading">
        <span className="sr-only">{t("detail.processes.loading")}</span>
        <Skeleton height={96} />
      </div>
    );
  }
  const root = site?.root ?? null;
  // `forge up|down <dir>` acts on the project folder (its forgebox.yaml), not on one process.
  const project = (
    <div
      role="group"
      aria-label={t("detail.processes.project")}
      className="flex flex-wrap items-center gap-2"
      data-testid="project-actions"
    >
      <ActionButton
        action={projectUp}
        input={root ?? ""}
        variant="primary"
        loadingLabel={t("detail.processes.projectStarting")}
        {...(root === null && { disabledReason: t("detail.general.noRoot") })}
        onDone={done}
      >
        {t("detail.processes.projectUp")}
      </ActionButton>
      <ActionButton
        action={projectDown}
        input={root ?? ""}
        loadingLabel={t("detail.processes.projectStopping")}
        {...(root === null && { disabledReason: t("detail.general.noRoot") })}
        onDone={done}
      >
        {t("detail.processes.projectDown")}
      </ActionButton>
    </div>
  );
  if (processes.length === 0) {
    return (
      <div className="flex flex-col gap-3">
        {project}
        <EmptyState
          title={t("detail.processes.empty")}
          description={t("detail.processes.emptyDescription")}
        />
      </div>
    );
  }
  const host = site?.host ?? siteId;
  return (
    <div className="flex max-w-3xl flex-col gap-3">
      {project}
      <ul aria-label={t("detail.processes.title")} className="flex flex-col gap-2">
        {processes.map((base) => {
          const row = { ...base, ...live[base.name] };
          const running = row.state === "running" || row.state === "starting";
          return (
            <li
              key={base.name}
              data-process-row={base.name}
              data-state={row.state}
              className="flex flex-wrap items-center gap-3 rounded-card border border-stroke bg-surface p-3"
            >
              <StatusDot state={DOT[row.state]} />
              <div className="min-w-0 flex-1">
                <p className="text-body font-semibold">{base.name}</p>
                <p className="truncate font-mono text-mono text-fg-secondary" title={base.command}>
                  {base.command}
                </p>
                <p className="text-caption text-fg-tertiary">
                  {[
                    t(`detail.processes.state.${row.state}`),
                    row.port === null ? null : t("detail.processes.port", { port: row.port }),
                    row.pid === null ? null : t("detail.processes.pid", { pid: row.pid }),
                    base.autostart ? t("detail.processes.autostart") : null,
                  ]
                    .filter((part) => part !== null)
                    .join(SEPARATOR)}
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <ActionButton
                  action={runSiteProcess}
                  input={{ site: host, process: base.name, action: "start" }}
                  variant="primary"
                  loadingLabel={t("detail.processes.starting")}
                  {...(running && { disabledReason: t("detail.processes.state.running") })}
                  onDone={done}
                >
                  {t("detail.processes.start")}
                </ActionButton>
                <ActionButton
                  action={runSiteProcess}
                  input={{ site: host, process: base.name, action: "stop" }}
                  loadingLabel={t("detail.processes.stopping")}
                  {...(!running && { disabledReason: t("detail.processes.state.stopped") })}
                  onDone={done}
                >
                  {t("detail.processes.stop")}
                </ActionButton>
                <ActionButton
                  action={runSiteProcess}
                  input={{ site: host, process: base.name, action: "restart" }}
                  loadingLabel={t("detail.processes.restarting")}
                  onDone={done}
                >
                  {t("detail.processes.restart")}
                </ActionButton>
                <Button
                  variant="ghost"
                  aria-pressed={openLogs === base.name}
                  aria-label={`${t("detail.processes.logs")}: ${base.name}`}
                  onClick={() => {
                    setOpenLogs(openLogs === base.name ? null : base.name);
                  }}
                >
                  {openLogs === base.name
                    ? t("detail.processes.hideLogs")
                    : t("detail.processes.logs")}
                </Button>
              </div>
              {openLogs === base.name ? (
                <div className="basis-full" data-testid="process-logs">
                  <LogViewer
                    targets={[`app:${host}/${base.name}`]}
                    name={`${host}/${base.name}`}
                    height={288}
                  />
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
