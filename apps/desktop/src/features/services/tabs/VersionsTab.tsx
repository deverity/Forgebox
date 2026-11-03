import { useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../../actions/registry";
import {
  checkToolUpdates,
  removeToolVersion,
  rollbackToolSeries,
  setToolPolicy,
  setCliVersion,
} from "../../../actions/tools";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { Badge } from "../../../design/primitives/Badge";
import { Button } from "../../../design/primitives/Button";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Select } from "../../../design/primitives/Select";
import { Tooltip } from "../../../design/primitives/Tooltip";
import { useFormatters } from "../../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../../i18n/format";
import { newIntent } from "../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../ipc/errors";
import type {
  ToolUpdatesDto,
  ToolVersionDto,
  UpdateHistoryEntryDto,
  UpdatePolicy,
} from "../../../ipc/generated";
import { useCapability, useForgeQuery, useInstances, useRuntime } from "../../../state";
import { QueryBoundary } from "./QueryBoundary";
import { exactSpec, inUse, rollbackCandidates, cliSpecFor, type InUse } from "./versionsModel";

export interface VersionsTabProps {
  /** Tool id (`php`, `mysql`). */
  tool: string;
}

const POLICIES: readonly UpdatePolicy[] = ["manual", "notify", "auto-patch", "auto-minor", "auto"];

function useInUseText() {
  const { t } = useTranslation("services");
  return (use: InUse): string => {
    switch (use.kind) {
      case "instance":
        return t("versions.inUseInstance", { names: use.names.join(", ") });
      case "sites":
        return t("versions.inUseSites", { count: use.count });
      case "cli":
        return t("versions.inUseCli");
    }
  };
}

interface VersionRowProps {
  version: ToolVersionDto;
  installed: readonly ToolVersionDto[];
  use: InUse | null;
}

function VersionRow({ version, installed, use }: VersionRowProps) {
  const { t } = useTranslation("services");
  const inUseText = useInUseText();
  const [confirming, setConfirming] = useState(false);
  const spec = exactSpec(version);
  const blocked = use === null ? undefined : inUseText(use);
  return (
    <li
      data-version={version.version}
      data-in-use={use === null ? undefined : use.kind}
      className="flex flex-col gap-2"
    >
      <div className="flex items-center gap-2">
        <span className={`text-body ${TNUM_CLASS}`}>{version.version}</span>
        {version.is_current ? <Badge tone="accent">{t("versions.cli")}</Badge> : null}
        {version.state === "broken" ? <Badge tone="danger">{t("versions.broken")}</Badge> : null}
        <span className="flex-1" />
        <ActionButton
          action={setCliVersion}
          input={{ tool: version.tool, version: cliSpecFor(version.version, installed) }}
          loadingLabel={t("versions.using")}
          successLabel={t("versions.used")}
          {...(version.is_current ? { disabledReason: t("versions.alreadyCli") } : {})}
        >
          {t("versions.useCli")}
        </ActionButton>
        <Tooltip
          content={blocked ?? t("versions.removeHint")}
          command={removeToolVersion.cli(spec) ?? ""}
        >
          <span
            data-disabled-reason={blocked}
            // The wrapper takes focus so keyboard users reach the reason of a disabled button.
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- same pattern as ActionButton
            tabIndex={blocked === undefined ? undefined : 0}
            className="inline-flex"
          >
            <Button
              variant="destructive"
              disabled={blocked !== undefined}
              aria-label={t("versions.removeVersion", { version: version.version })}
              onClick={() => {
                setConfirming(!confirming);
              }}
            >
              {t("versions.remove")}
            </Button>
          </span>
        </Tooltip>
      </div>
      {confirming && blocked === undefined ? (
        <div
          role="group"
          aria-label={t("versions.removeTitle", { spec })}
          data-testid="remove-version-confirm"
          className="flex flex-col gap-2 rounded-card border border-state-failed p-3"
        >
          <p className="text-callout">{t("versions.removeWarning", { spec })}</p>
          <div className="flex gap-2">
            <ActionButton
              variant="destructive"
              action={removeToolVersion}
              input={spec}
              loadingLabel={t("versions.removing")}
              successLabel={t("versions.removed")}
              onDone={() => {
                setConfirming(false);
              }}
            >
              {t("versions.confirmRemove")}
            </ActionButton>
            <Button
              variant="ghost"
              onClick={() => {
                setConfirming(false);
              }}
            >
              {t("versions.cancel")}
            </Button>
          </div>
        </div>
      ) : null}
    </li>
  );
}

function HistoryRow({
  tool,
  entry,
  canRollback,
}: {
  tool: string;
  entry: UpdateHistoryEntryDto;
  canRollback: boolean;
}) {
  const { t } = useTranslation("services");
  const format = useFormatters();
  const series = entry.series ?? "";
  return (
    <li data-history={entry.id} className="flex items-center gap-2 text-callout">
      <span className={`min-w-0 flex-1 ${TNUM_CLASS}`}>
        {t("versions.historyLine", {
          from: entry.from ?? "—",
          to: entry.to,
          status: t(`versions.status.${entry.status}`),
        })}
        <span className="block text-caption text-fg-tertiary">
          {format.date(new Date(entry.started_at), { dateStyle: "short", timeStyle: "short" })}
        </span>
      </span>
      {canRollback && series !== "" ? (
        <ActionButton
          action={rollbackToolSeries}
          input={`${tool}@${series}`}
          loadingLabel={t("versions.rollingBack")}
          successLabel={t("versions.rolledBack")}
        >
          {t("versions.rollback")}
        </ActionButton>
      ) : null}
    </li>
  );
}

const SEPARATOR = " · ";

function UpdateResult({ found }: { found: ToolUpdatesDto }) {
  const { t } = useTranslation("services");
  const text =
    found.updates.length === 0
      ? t("versions.upToDate")
      : found.updates
          .map((u) => t("versions.available", { from: u.from, to: u.to }))
          .join(SEPARATOR);
  return (
    <p data-testid="update-result" className="text-callout text-fg-secondary">
      {text}
    </p>
  );
}

function PolicyPicker({ tool, policy }: { tool: string; policy: UpdatePolicy }) {
  const { t } = useTranslation("services");
  const runtime = useRuntime();
  const capability = useCapability("tools.policy");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const options = POLICIES.map((value) => ({ value, label: t(`versions.policies.${value}`) }));
  return (
    <div className="flex flex-col gap-1" data-testid="policy-picker">
      <span className="text-callout text-fg-secondary" aria-hidden="true">
        {t("versions.policy")}
      </span>
      <Select
        label={t("versions.policy")}
        options={options}
        value={policy}
        disabled={busy || !capability.available}
        onValueChange={(next) => {
          setBusy(true);
          setFailure(null);
          runAction(
            runtime.queue,
            setToolPolicy,
            { tool, policy: next as UpdatePolicy },
            { intent: newIntent() },
          ).then(
            () => {
              setBusy(false);
            },
            (error: unknown) => {
              setBusy(false);
              setFailure(toForgeApiError(error, { method: setToolPolicy.method }));
            },
          );
        }}
      />
      <p className="text-caption text-fg-tertiary">{t(`versions.policyHint.${policy}`)}</p>
      {failure === null ? null : <ErrorNotice error={failure} />}
    </div>
  );
}

/** Versoes: installed versions, history with rollback, update check and update policy. */
export function VersionsTab({ tool }: VersionsTabProps) {
  const { t } = useTranslation("services");
  const info = useForgeQuery("tools.info", { tool });
  const history = useForgeQuery("tools.history", { tool, limit: 20 });
  const instances = useInstances();
  const [found, setFound] = useState<ToolUpdatesDto | null>(null);
  return (
    <div className="flex flex-col gap-5">
      <QueryBoundary query={info} height={160}>
        {(dto) => {
          const installed = dto.tool.installed;
          const all = Object.values(instances).filter((i) => i.tool === tool);
          return (
            <>
              <section aria-labelledby="versions-installed" className="flex flex-col gap-2">
                <h3 id="versions-installed" className="text-headline">
                  {t("versions.installed")}
                </h3>
                <p className="text-callout text-fg-secondary">{t("versions.cliExplain")}</p>
                {installed.length === 0 ? (
                  <p className="text-callout text-fg-secondary">{t("versions.none")}</p>
                ) : (
                  <ul className="flex flex-col gap-3">
                    {installed.map((version) => (
                      <VersionRow
                        key={version.id}
                        version={version}
                        installed={installed}
                        use={inUse(version, all, dto.used_by)}
                      />
                    ))}
                  </ul>
                )}
              </section>
              <section aria-labelledby="versions-updates" className="flex flex-col gap-3">
                <h3 id="versions-updates" className="text-headline">
                  {t("versions.updates")}
                </h3>
                <PolicyPicker tool={tool} policy={dto.tool.policy} />
                <div className="flex flex-col gap-2">
                  <div>
                    <ActionButton
                      action={checkToolUpdates}
                      input={tool}
                      loadingLabel={t("versions.checking")}
                      successLabel={t("versions.checked")}
                      onDone={(result) => {
                        setFound(result as ToolUpdatesDto);
                      }}
                    >
                      {t("versions.check")}
                    </ActionButton>
                  </div>
                  {found === null ? null : <UpdateResult found={found} />}
                </div>
              </section>
            </>
          );
        }}
      </QueryBoundary>
      <QueryBoundary query={history} height={80}>
        {(dto) => {
          const candidates = rollbackCandidates(dto.entries);
          return (
            <section aria-labelledby="versions-history" className="flex flex-col gap-2">
              <h3 id="versions-history" className="text-headline">
                {t("versions.history")}
              </h3>
              {dto.entries.length === 0 ? (
                <p className="text-callout text-fg-secondary">{t("versions.historyEmpty")}</p>
              ) : (
                <ul className="flex flex-col gap-2">
                  {dto.entries.map((entry) => (
                    <HistoryRow
                      key={entry.id}
                      tool={tool}
                      entry={entry}
                      canRollback={candidates.get(entry.series ?? "")?.id === entry.id}
                    />
                  ))}
                </ul>
              )}
            </section>
          );
        }}
      </QueryBoundary>
    </div>
  );
}
