import { useState } from "react";
import { useTranslation } from "react-i18next";
import { addRegistry, removeRegistry, updateRegistries } from "../../../../actions/filesystem";
import { runAction } from "../../../../actions/registry";
import { ActionButton } from "../../../../design/primitives/ActionButton";
import { Badge, type BadgeTone } from "../../../../design/primitives/Badge";
import { Button } from "../../../../design/primitives/Button";
import { EmptyState } from "../../../../design/primitives/EmptyState";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { TextField } from "../../../../design/primitives/TextField";
import { useFormatters } from "../../../../design/primitives/useFormatters";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { RegistryDto } from "../../../../ipc/generated";
import { useCapability, useForgeQuery, useRuntime } from "../../../../state";
import { loose } from "../../looseT";
import { matchesSearch } from "../../model";
import type { SectionProps } from "../../registry";
import { RowFrame } from "../../RowFrame";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";
import { registryTrust, urlProblem, type RegistryTrust } from "./model";

const TID_REGISTRY_REMOVE_DIALOG = "registry-remove-dialog";
const TID_REGISTRY_UPDATE = "registry-update";
const TID_REGISTRY_UPDATE_STATUS = "registry-update-status";
const TID_REGISTRY_ITEMS = "registry-items";
const SEPARATOR = " · ";
const METHOD_REMOVE = "registry.remove";
const URL_EXAMPLE = "https://example.org/registry/index.yaml";

/** Row ids of the section (`registries.<id>.label` in the `settings` namespace). */
export const REGISTRIES_ROWS = ["list", "add"] as const;

const METHOD_LIST = "registry.list";
const METHOD_UPDATE = "registry.update";
const OFFICIAL = "official";

const TONE: Record<RegistryTrust, BadgeTone> = {
  official: "success",
  "third-party": "info",
  unverified: "warning",
};

function RegistryRow({ registry }: { registry: RegistryDto }) {
  const { t } = useTranslation("settings");
  const format = useFormatters();
  const runtime = useRuntime();
  const capability = useCapability("registry.remove");
  const [ask, setAsk] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const trust = registryTrust(registry);
  const removable = registry.name !== OFFICIAL;

  const remove = () => {
    setAsk(false);
    setBusy(true);
    setFailure(null);
    runAction(runtime.queue, removeRegistry, registry.name, { intent: newIntent() }).then(
      () => {
        setBusy(false);
      },
      (error: unknown) => {
        setBusy(false);
        setFailure(toForgeApiError(error, { method: METHOD_REMOVE }));
      },
    );
  };

  return (
    <li
      data-testid={`registry-${registry.name}`}
      data-trust={trust}
      className="flex flex-col gap-2 py-3"
    >
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-body font-medium">{registry.name}</span>
        <Badge tone={TONE[trust]}>{loose(t)(`registries.trust.${trust}`)}</Badge>
        {registry.enabled ? null : <Badge>{t("registries.disabled")}</Badge>}
        <span className="min-w-40 flex-1 font-mono text-mono text-fg-secondary [overflow-wrap:anywhere]">
          {registry.url}
        </span>
        {removable ? (
          <Button
            variant="destructive"
            loading={busy}
            loadingLabel={t("registries.removing")}
            disabled={!capability.available}
            data-testid={`registry-remove-${registry.name}`}
            data-forge-cli={removeRegistry.cli(registry.name) ?? undefined}
            onClick={() => {
              setAsk(true);
            }}
          >
            {t("registries.remove")}
          </Button>
        ) : null}
      </div>
      <p className="text-callout text-fg-secondary">
        {t("registries.tools", { count: registry.tools })}
        {SEPARATOR}
        <span data-testid={`registry-refresh-${registry.name}`}>
          {registry.last_refresh_at === null
            ? t("registries.neverRefreshed")
            : t("registries.refreshed", {
                when: format.relative(new Date(registry.last_refresh_at)),
              })}
        </span>
      </p>
      {registry.last_refresh_at === null && registry.enabled ? (
        <p
          data-testid={`registry-error-${registry.name}`}
          role="alert"
          className="text-callout text-danger"
        >
          {t("registries.noRefresh")}
        </p>
      ) : null}
      {failure === null ? null : (
        <ErrorNotice error={failure} context={{ method: METHOD_REMOVE }} />
      )}
      <ConfirmDialog
        open={ask}
        onOpenChange={setAsk}
        title={t("registries.removeTitle", { name: registry.name })}
        description={t("registries.removeBody")}
        confirmLabel={t("registries.remove")}
        command={removeRegistry.cli(registry.name)}
        destructive
        testId={TID_REGISTRY_REMOVE_DIALOG}
        onConfirm={remove}
      />
    </li>
  );
}

function AddForm() {
  const { t } = useTranslation("settings");
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const problem = urlProblem(url);
  const message = problem === undefined ? undefined : loose(t)(`registries.problem.${problem}`);
  const input = { url: url.trim(), ...(name.trim() === "" ? {} : { name: name.trim() }) };
  return (
    <RowFrame
      id="registry-add"
      label={t("registries.add.label")}
      description={t("registries.add.description")}
    >
      <div className="flex w-full min-w-96 flex-col gap-3">
        <TextField
          label={t("registries.add.url")}
          width="full"
          value={url}
          placeholder={URL_EXAMPLE}
          validate={(value) => {
            const found = urlProblem(value);
            return found === undefined ? undefined : loose(t)(`registries.problem.${found}`);
          }}
          onChange={(event) => {
            setUrl(event.target.value);
          }}
        />
        <TextField
          label={t("registries.add.name")}
          hint={t("registries.add.nameHint")}
          value={name}
          onChange={(event) => {
            setName(event.target.value);
          }}
        />
        <div>
          <ActionButton
            action={addRegistry}
            input={input}
            variant="primary"
            {...(message === undefined ? {} : { disabledReason: message })}
            loadingLabel={t("registries.add.adding")}
            onDone={() => {
              setUrl("");
              setName("");
            }}
          >
            {t("registries.add.submit")}
          </ActionButton>
        </div>
      </div>
    </RowFrame>
  );
}

/**
 * Registries (docs/04 section 5): the sources the catalog reads, with their trust badge and last
 * refresh; add by URL (checked inline), remove after a confirmation and "Atualizar" as a job.
 */
export function RegistriesSection({ query }: SectionProps) {
  const { t } = useTranslation("settings");
  const list = useForgeQuery(METHOD_LIST, {});
  const update = useJobAction(updateRegistries);
  const show = (id: (typeof REGISTRIES_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`registries.${id}.label`));
  const registries = list.data?.registries ?? [];
  return (
    <div className="flex flex-col">
      {show("list") ? (
        <RowFrame
          id="registry-list"
          label={t("registries.list.label")}
          description={t("registries.list.description")}
        >
          <div className="flex w-full min-w-96 flex-col gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <JobButton
                action={updateRegistries}
                input={undefined}
                job={update}
                testId={TID_REGISTRY_UPDATE}
              >
                {t("registries.update")}
              </JobButton>
            </div>
            <JobStatus
              job={update}
              label={t("registries.updating")}
              method={METHOD_UPDATE}
              testId={TID_REGISTRY_UPDATE_STATUS}
            />
            {list.isError ? (
              <ErrorNotice
                error={list.error}
                context={{ method: METHOD_LIST }}
                onRetry={() => {
                  void list.refetch();
                }}
              />
            ) : registries.length === 0 && !list.isPending ? (
              <EmptyState title={t("registries.empty")} />
            ) : (
              <ul data-testid={TID_REGISTRY_ITEMS} className="divide-y divide-stroke">
                {registries.map((registry) => (
                  <RegistryRow key={registry.name} registry={registry} />
                ))}
              </ul>
            )}
          </div>
        </RowFrame>
      ) : null}
      {show("add") ? <AddForm /> : null}
    </div>
  );
}
