import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { AsyncView } from "../../design/primitives/AsyncView";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import type { ForgeApiError } from "../../ipc/errors";
import { useCapability, useConnectionStatus, useForgeQuery, useRuntime } from "../../state";
import { CreateForm } from "./CreateForm";
import { DatabaseTable } from "./DatabaseTable";
import { InstanceCard } from "./InstanceCard";
import { databasesOf, pickInstance } from "./model";
import { TransferList } from "./TransferList";

const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const LIST_CONTEXT = { method: "db.list", params: {} };
const CATALOG_PATH = "/catalog";
/** Height AsyncView reserves for loading, error, empty and disconnected only. */
const STATE_HEIGHT = 320;

/**
 * Route `/databases` (docs/07 §5.6): the database instances as cards, the databases of the
 * chosen one with create / drop / export / import, connection strings and a shell. Redis,
 * users and scheduled backups are M3-E1 and do not appear.
 */
export function DatabasesPage() {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const navigate = useNavigate();
  const status = useConnectionStatus();
  const list = useForgeQuery("db.list", {});
  const capability = useCapability("db.list");
  const reason = useCapabilityReason(capability);
  const [chosen, setChosen] = useState<string | null>(null);
  const [notice, setNotice] = useState<ForgeApiError | null>(null);
  const instances = list.data?.instances ?? [];
  const away = AWAY.has(status);
  // R-16: a daemon that answers `not_implemented` never runs the query (it stays pending), so
  // say so instead of an endless skeleton.
  const unavailable = !capability.available && capability.reason !== "disconnected";
  const current = pickInstance(instances, chosen);
  const databases =
    current === undefined ? [] : databasesOf(list.data?.databases ?? [], current.instance);
  const caps = current?.capabilities ?? {};
  const ready = !away && !list.isPending && !list.isError && instances.length > 0;
  return (
    <div
      data-testid="page-databases"
      className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4"
    >
      <header className="flex items-baseline justify-between gap-3">
        <h1 className="text-title-1">{t("title")}</h1>
        {instances.length === 0 ? null : (
          <p className="text-callout text-fg-secondary">
            {t("instanceCount", { count: instances.length })}
          </p>
        )}
      </header>
      {ready ? (
        <div className="flex flex-col gap-4">
          <div
            role="radiogroup"
            aria-label={t("instances")}
            data-testid="instance-cards"
            className="flex gap-3 overflow-x-auto pb-1"
          >
            {instances.map((instance) => (
              <InstanceCard
                key={instance.instance}
                instance={instance}
                selected={current?.instance === instance.instance}
                onSelect={() => {
                  setChosen(instance.instance);
                  setNotice(null);
                }}
              />
            ))}
          </div>
          {current === undefined ? null : current.state !== "running" ? (
            <EmptyState
              title={t("stopped.title", { name: current.instance })}
              description={t("stopped.description")}
            />
          ) : (
            <>
              {caps.create === false ? null : (
                <CreateForm
                  instance={current.instance}
                  protocol={current.protocol}
                  existing={databases.map((db) => db.name)}
                />
              )}
              {notice === null ? null : <ErrorNotice error={notice} />}
              <DatabaseTable instance={current} databases={databases} onError={setNotice} />
            </>
          )}
          <TransferList />
        </div>
      ) : (
        <AsyncView
          height={STATE_HEIGHT}
          loading={!away && !unavailable && list.isPending}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(list.isError ? { error: list.error } : {})}
          errorContext={LIST_CONTEXT}
          onRetry={() => {
            void list.refetch();
          }}
          empty={unavailable || instances.length === 0}
          emptyTitle={unavailable && reason !== null ? reason : t("empty.title")}
          {...(unavailable ? {} : { emptyDescription: t("empty.description") })}
          emptyAction={
            unavailable ? undefined : (
              <Button
                variant="primary"
                onClick={() => {
                  void navigate({ to: CATALOG_PATH });
                }}
              >
                {t("empty.action")}
              </Button>
            )
          }
        >
          <span />
        </AsyncView>
      )}
    </div>
  );
}
