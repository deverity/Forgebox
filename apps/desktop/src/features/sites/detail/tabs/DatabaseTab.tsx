import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { startInstance } from "../../../../actions/services";
import { ActionButton } from "../../../../design/primitives/ActionButton";
import { AsyncView } from "../../../../design/primitives/AsyncView";
import { Select } from "../../../../design/primitives/Select";
import { FOCUS_RING } from "../../../../design/primitives/shared";
import { useConnectionStatus, useForgeQuery, useRuntime, useSite } from "../../../../state";
import { ConnectionStrings } from "../../../databases/ConnectionPopover";
import { CreateForm } from "../../../databases/CreateForm";
import { databasesOf, pickInstance, suggestDatabaseName } from "../../../databases/model";
import { TransferList } from "../../../databases/TransferList";

const AWAY: ReadonlySet<string> = new Set(["stopped", "closed", "uninitialized"]);
const LIST_CONTEXT = { method: "db.list", params: {} };
const STATE_HEIGHT = 160;
const CATALOG_PATH = "/catalog";
const DATABASES_PATH = "/databases";

/**
 * "Banco": creates the database of the site (named after its host, `loja.test` to `loja`) on
 * a database instance and shows its connection strings. The full management is the Databases
 * screen, one link away (docs/07 §5.4, §5.6).
 */
export function DatabaseTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const status = useConnectionStatus();
  const site = useSite(siteId);
  const list = useForgeQuery("db.list", {});
  const [chosen, setChosen] = useState<string | null>(null);
  const instances = list.data?.instances ?? [];
  const away = AWAY.has(status);
  const current = pickInstance(instances, chosen);
  const wanted = site === undefined ? "" : suggestDatabaseName(site.host);
  const databases =
    current === undefined ? [] : databasesOf(list.data?.databases ?? [], current.instance);
  const exists = wanted !== "" && databases.some((db) => db.name === wanted);
  const ready = !away && !list.isPending && !list.isError && instances.length > 0;
  return (
    <div data-testid="site-database-tab" className="flex flex-col gap-4">
      <header className="flex flex-col gap-1">
        <h2 className="text-headline">{t("siteTab.title", { host: site?.host ?? "" })}</h2>
        <p className="text-callout text-fg-secondary">{t("siteTab.description")}</p>
      </header>
      {!ready || current === undefined ? (
        <AsyncView
          height={STATE_HEIGHT}
          loading={!away && list.isPending}
          disconnected={away}
          onStartDaemon={() => {
            void runtime.api.start();
          }}
          {...(list.isError ? { error: list.error } : {})}
          errorContext={LIST_CONTEXT}
          onRetry={() => {
            void list.refetch();
          }}
          empty={instances.length === 0}
          emptyTitle={t("siteTab.noInstance")}
          emptyAction={
            <Link to={CATALOG_PATH} className="text-accent underline">
              {t("siteTab.noInstanceAction")}
            </Link>
          }
        >
          <span />
        </AsyncView>
      ) : (
        <div className="flex flex-col gap-4">
          {instances.length > 1 ? (
            <Select
              label={t("siteTab.instance")}
              options={instances.map((i) => ({ value: i.instance, label: i.instance }))}
              value={current.instance}
              onValueChange={setChosen}
            />
          ) : null}
          {current.state !== "running" ? (
            <div className="flex flex-col items-start gap-2">
              <p className="text-callout text-fg-secondary">
                {t("siteTab.stopped", { name: current.instance })}
              </p>
              <ActionButton
                action={startInstance}
                input={current.instance}
                variant="primary"
                loadingLabel={t("instance.starting")}
              >
                {t("siteTab.start")}
              </ActionButton>
            </div>
          ) : exists ? (
            <section aria-label={t("siteTab.connection")} className="flex flex-col gap-2">
              <p role="status" className="text-callout">
                {t("siteTab.exists", { name: wanted })}
              </p>
              <ConnectionStrings
                target={{
                  instance: current.instance,
                  protocol: current.protocol,
                  database: wanted,
                }}
              />
            </section>
          ) : (
            <CreateForm
              key={`${current.instance}:${wanted}`}
              instance={current.instance}
              protocol={current.protocol}
              existing={databases.map((db) => db.name)}
              initialName={wanted}
            />
          )}
          <TransferList instance={current.instance} />
          <Link to={DATABASES_PATH} className={`text-callout text-accent underline ${FOCUS_RING}`}>
            {t("siteTab.open")}
          </Link>
        </div>
      )}
    </div>
  );
}
