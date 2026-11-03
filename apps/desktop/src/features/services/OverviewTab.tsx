import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { killPortHolder, setServicePort } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { PortField, parsePort } from "../../design/primitives/PortField";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { ErrorAction } from "../../ipc/describeError";
import type { InstanceDto } from "../../ipc/generated";
import { useForgeQuery } from "../../state";
import { isFailure, mainPort, portHolder, targetOf } from "./model";

export interface OverviewTabProps {
  instance: InstanceDto;
  /** Switch the sheet to the Logs tab (the `service_logs` action of a failure). */
  onShowLogs: () => void;
}

/** Timeline entries kept (newest first). */
const TIMELINE_LIMIT = 12;

function Timeline({ instance }: { instance: InstanceDto }) {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const history = useForgeQuery("events.list", {
    after_seq: 0,
    limit: 500,
    filter: { kinds: ["service.*"], instance_id: instance.id },
  });
  const { refetch } = history;
  // The live state changed: the timeline gained an entry (events do not invalidate this query).
  const stamp = `${instance.state}/${String(instance.restarts)}`;
  const seen = useRef(stamp);
  useEffect(() => {
    if (seen.current === stamp) return;
    seen.current = stamp;
    void refetch();
  }, [stamp, refetch]);
  const events = [...(history.data?.events ?? [])].reverse().slice(0, TIMELINE_LIMIT);
  return (
    <section aria-labelledby="timeline-title" className="flex flex-col gap-2">
      <h3 id="timeline-title" className="text-headline">
        {t("overview.timeline")}
      </h3>
      {events.length === 0 ? (
        <p className="text-callout text-fg-secondary">
          {history.isPending ? t("overview.timelineLoading") : t("overview.timelineEmpty")}
        </p>
      ) : (
        <ol data-testid="timeline" className="flex flex-col gap-1">
          {events.map((event) => (
            <li
              key={event.id}
              data-event-kind={event.kind}
              className="flex items-baseline justify-between gap-3 text-callout"
            >
              <span className="text-fg-primary">
                {t(`timeline.${event.kind.replaceAll(".", "_")}`, { defaultValue: event.kind })}
              </span>
              <time dateTime={event.ts} className={`text-fg-secondary ${TNUM_CLASS}`}>
                {format.date(Date.parse(event.ts), { dateStyle: "short", timeStyle: "medium" })}
              </time>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

function ChangePort({ target, onDone }: { target: string; onDone: () => void }) {
  const { t } = useTranslation("services");
  const [value, setValue] = useState("");
  const port = parsePort(value);
  return (
    <div data-testid="change-port" className="flex flex-col gap-2">
      <PortField label={t("failure.newPort")} value={value} onChange={setValue} />
      {port === null ? (
        <p className="text-caption text-fg-tertiary">{t("failure.portCliHint")}</p>
      ) : (
        <CliCommand command={setServicePort.cli({ target, port })} />
      )}
      <div>
        <ActionButton
          variant="primary"
          action={setServicePort}
          input={{ target, port: port ?? 0 }}
          loadingLabel={t("failure.applying")}
          successLabel={t("failure.applied")}
          {...(port === null ? { disabledReason: t("failure.portInvalid") } : {})}
          onDone={onDone}
        >
          {t("failure.apply")}
        </ActionButton>
      </div>
    </div>
  );
}

/** The failure card: localized cause, the actions of `describeError` and the way out. */
function FailureCard({ instance, onShowLogs }: OverviewTabProps) {
  const { t } = useTranslation("services");
  const [changing, setChanging] = useState(false);
  const target = targetOf(instance);
  const error = instance.last_error;
  if (!isFailure(instance.state) || error === null) return null;
  const holder = portHolder(error);
  const handled = new Set<string>(["change_port", "service_logs"]);
  const onAction = (action: ErrorAction) => {
    if (action.id === "change_port") setChanging(true);
    else if (action.id === "service_logs") onShowLogs();
  };
  return (
    <section
      data-testid="failure-card"
      aria-label={t("failure.title")}
      className="flex flex-col gap-3 rounded-card border border-state-failed p-3"
    >
      <h3 className="text-headline text-state-failed">{t("failure.title")}</h3>
      <ErrorNotice
        error={error}
        context={{ target }}
        onAction={onAction}
        canHandle={(action) => handled.has(action.id)}
      />
      {holder === null ? null : (
        <div>
          <ActionButton
            variant="destructive"
            action={killPortHolder}
            input={{ port: holder.port, pid: holder.pid }}
            loadingLabel={t("failure.killing")}
            successLabel={t("failure.killed")}
          >
            {t("failure.kill", { exe: holder.exe ?? String(holder.pid), pid: holder.pid })}
          </ActionButton>
        </div>
      )}
      {changing ? (
        <ChangePort
          target={target}
          onDone={() => {
            setChanging(false);
          }}
        />
      ) : null}
      {changing ? (
        <Button
          variant="ghost"
          onClick={() => {
            setChanging(false);
          }}
        >
          {t("failure.cancelPort")}
        </Button>
      ) : null}
    </section>
  );
}

export function OverviewTab({ instance, onShowLogs }: OverviewTabProps) {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const port = mainPort(instance);
  const facts: [string, string][] = [
    [t("overview.version"), instance.version],
    [
      t("overview.port"),
      port === null ? t("row.noPort") : instance.ports.map((p) => p.port).join(", "),
    ],
    [t("overview.pid"), instance.pid === null ? "—" : String(instance.pid)],
    [t("overview.restarts"), String(instance.restarts)],
    [t("overview.autostart"), instance.autostart ? t("overview.yes") : t("overview.no")],
  ];
  if (instance.data_dir != null) facts.push([t("overview.dataDir"), instance.data_dir]);
  if (instance.size_bytes != null)
    facts.push([t("overview.size"), format.bytes(instance.size_bytes)]);
  return (
    <div className="flex flex-col gap-4">
      <FailureCard instance={instance} onShowLogs={onShowLogs} />
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-callout">
        {facts.map(([label, value]) => (
          <div key={label} className="contents">
            <dt className="text-fg-secondary">{label}</dt>
            <dd className={`truncate text-fg-primary ${TNUM_CLASS}`} title={value}>
              {value}
            </dd>
          </div>
        ))}
      </dl>
      <Timeline instance={instance} />
    </div>
  );
}
