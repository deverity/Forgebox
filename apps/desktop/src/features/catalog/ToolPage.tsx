import { Link } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { removeToolVersion, installTools } from "../../actions/tools";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Badge } from "../../design/primitives/Badge";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Select } from "../../design/primitives/Select";
import { FOCUS_RING } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import type { ToolInfoDto, ToolReleaseDto, ToolVersionDto } from "../../ipc/generated";
import type { ErrorContext } from "../../ipc/describeError";
import { openUrl } from "../../ipc/native";
import {
  NOT_IMPLEMENTED_ERROR,
  useCapability,
  useConnectionStatus,
  useForgeQuery,
  useRuntime,
} from "../../state";
import { PolicyPicker } from "../services/tabs/VersionsTab";
import { InstallProgressList } from "../shared/install-progress";
import { CatalogIcon } from "./CatalogIcon";
import { useJobQuery } from "./hooks";
import {
  defaultRelease,
  groupBySeries,
  installSpec,
  latestOf,
  planSpecs,
  safeHttpUrl,
  summarizePlan,
  variantChoice,
} from "./model";
import { LINK_CLASS } from "./links";
import { PlanSummaryView } from "./PlanSummaryView";
import { SafeMarkdown } from "./SafeMarkdown";
import { EntryBadges } from "./ToolCard";
import { useInstallRunner, type InstallRunner } from "./useInstallRunner";

const CATALOG_ROUTE = "/catalog";
/** Room for a stack plan of three or four items, so the confirm button does not jump. */
const STACK_PLAN_HEIGHT = 180;

function ExternalAnchor({ url, children }: { url: string; children: string }) {
  return (
    <a
      href={url}
      rel="noopener noreferrer"
      className={LINK_CLASS}
      onClick={(event) => {
        event.preventDefault();
        void openUrl(url);
      }}
    >
      {children}
    </a>
  );
}

// --- installed versions ------------------------------------------------------------------

function InstalledRow({ version }: { version: ToolVersionDto }) {
  const { t } = useTranslation("catalog");
  const [confirming, setConfirming] = useState(false);
  const spec = `${version.tool}@${version.version}`;
  return (
    <li data-installed-version={version.version} className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className={`text-body ${TNUM_CLASS}`}>{version.version}</span>
        {version.is_current ? <Badge tone="accent">{t("tool.current")}</Badge> : null}
        {version.state === "broken" ? <Badge tone="danger">{t("tool.broken")}</Badge> : null}
        <span className="flex-1" />
        <Button
          variant="destructive"
          aria-label={t("tool.removeVersion", { version: version.version })}
          onClick={() => {
            setConfirming(!confirming);
          }}
        >
          {t("tool.remove")}
        </Button>
      </div>
      {confirming ? (
        <div
          role="group"
          aria-label={t("tool.removeTitle", { spec })}
          className="flex flex-col gap-2 rounded-card border border-state-failed p-3"
        >
          <p className="text-callout">{t("tool.removeWarning", { spec })}</p>
          <div className="flex gap-2">
            <ActionButton
              variant="destructive"
              action={removeToolVersion}
              input={spec}
              loadingLabel={t("tool.removing")}
              successLabel={t("tool.removed")}
              onDone={() => {
                setConfirming(false);
              }}
            >
              {t("tool.confirmRemove")}
            </ActionButton>
            <Button
              variant="ghost"
              onClick={() => {
                setConfirming(false);
              }}
            >
              {t("tool.cancel")}
            </Button>
          </div>
        </div>
      ) : null}
    </li>
  );
}

function InstalledSection({ info }: { info: ToolInfoDto }) {
  const { t } = useTranslation("catalog");
  const installed = info.tool.installed;
  return (
    <section aria-labelledby="catalog-installed" className="flex flex-col gap-2">
      <h2 id="catalog-installed" className="text-title-2">
        {t("tool.installed")}
      </h2>
      {installed.length === 0 ? (
        <p className="text-callout text-fg-secondary">{t("tool.noneInstalled")}</p>
      ) : (
        <ul className="flex flex-col gap-3">
          {installed.map((version) => (
            <InstalledRow key={version.id} version={version} />
          ))}
        </ul>
      )}
      {installed.length === 0 ? null : (
        <PolicyPicker tool={info.tool.id} policy={info.tool.policy} />
      )}
    </section>
  );
}

// --- releases and install ----------------------------------------------------------------

function ReleaseRow({
  release,
  selected,
  onChoose,
}: {
  release: ToolReleaseDto;
  selected: boolean;
  onChoose: (version: string) => void;
}) {
  const { t } = useTranslation("catalog");
  const format = useFormatters();
  return (
    <li
      data-release={release.version}
      data-selected={selected ? "true" : undefined}
      className="flex items-center gap-2 text-callout"
    >
      <span className={`w-28 ${TNUM_CLASS}`}>{release.version}</span>
      {release.channel !== undefined && release.channel !== null && release.channel !== "stable" ? (
        <Badge tone="warning">{release.channel}</Badge>
      ) : null}
      {release.installed === true ? <Badge tone="accent">{t("tool.installedBadge")}</Badge> : null}
      <span className={`flex-1 text-fg-secondary ${TNUM_CLASS}`}>
        {release.size_bytes === null || release.size_bytes === undefined
          ? ""
          : format.bytes(release.size_bytes)}
      </span>
      <Button
        variant="ghost"
        aria-pressed={selected}
        aria-label={t(selected ? "tool.chosenLabel" : "tool.choose", { version: release.version })}
        onClick={() => {
          onChoose(release.version);
        }}
      >
        {selected ? t("tool.chosen") : t("tool.chooseShort")}
      </Button>
    </li>
  );
}

function InstallSection({ info, runner }: { info: ToolInfoDto; runner: InstallRunner }) {
  const { t } = useTranslation("catalog");
  const format = useFormatters();
  const tool = info.tool.id;
  const releases = useJobQuery("tools.releases", { tool });
  const releasesCapability = useCapability("tools.releases");
  const releasesReason = useCapabilityReason(releasesCapability);
  const installCapability = useCapability("tools.install");
  const installReason = useCapabilityReason(installCapability);

  const list = useMemo(() => releases.data?.releases ?? [], [releases.data]);
  const groups = useMemo(() => groupBySeries(list), [list]);
  const [picked, setPicked] = useState("");
  const [variant, setVariant] = useState("");

  // The user's pick, else the newest release not installed yet (derived: no effect needed).
  const version = list.some((candidate) => candidate.version === picked)
    ? picked
    : (defaultRelease(list)?.version ?? "");
  const setVersion = setPicked;
  const release = list.find((candidate) => candidate.version === version);
  const choice = variantChoice(info.variants ?? [], release);
  const chosenVariant =
    choice === null ? null : choice.values.includes(variant) ? variant : choice.default;

  const spec = version === "" ? null : installSpec(tool, version);
  const plan = useJobQuery(
    "tools.plan",
    { specs: spec === null ? [] : [spec], variant: chosenVariant },
    { enabled: spec !== null },
  );

  const isExtension = info.kind === "extension";
  const reason = isExtension
    ? t("tool.extensionNote", { target: info.extends ?? "" })
    : (installReason ?? (spec === null ? t("tool.pickVersion") : undefined));
  const request =
    spec === null
      ? null
      : {
          specs: [spec],
          variant: chosenVariant,
          label:
            chosenVariant === null
              ? `${info.tool.name} ${version}`
              : `${info.tool.name} ${version} (${chosenVariant})`,
        };
  const cli = spec === null ? null : installTools.cli({ specs: [spec], variant: chosenVariant });

  let body;
  if (releasesReason !== null && releases.data === undefined) {
    body = <p className="text-callout text-fg-secondary">{releasesReason}</p>;
  } else if (releases.isError) {
    body = (
      <ErrorNotice
        error={releases.error}
        onRetry={() => {
          void releases.refetch();
        }}
      />
    );
  } else if (releases.data === undefined) {
    body = (
      <div aria-busy="true" data-testid="releases-loading">
        <span className="sr-only">{t("tool.loadingReleases")}</span>
        <Skeleton height={96} />
      </div>
    );
  } else {
    body = (
      <>
        {releases.data.stale === true ? (
          <div
            role="status"
            data-testid="stale-banner"
            className="flex flex-wrap items-center gap-2 rounded-card border border-state-starting p-3 text-callout"
          >
            <span className="min-w-0 flex-1">
              {releases.data.fetched_at === undefined || releases.data.fetched_at === null
                ? t("tool.stale")
                : t("tool.staleSince", {
                    date: format.date(new Date(releases.data.fetched_at), {
                      dateStyle: "short",
                      timeStyle: "short",
                    }),
                  })}
            </span>
            <Button
              variant="secondary"
              onClick={() => {
                void releases.refetch();
              }}
            >
              {t("tool.retry")}
            </Button>
          </div>
        ) : null}
        {groups.length === 0 ? (
          <p className="text-callout text-fg-secondary">{t("tool.noReleases")}</p>
        ) : (
          <div className="flex flex-col gap-2">
            {groups.map((group) => (
              <section
                key={group.series}
                aria-label={t("tool.series", {
                  series: group.series,
                  count: group.releases.length,
                })}
                data-series={group.series}
                className="rounded-card border border-stroke px-3 py-2"
              >
                <h3 className="text-body font-semibold">
                  {t("tool.series", { series: group.series, count: group.releases.length })}
                </h3>
                <ul className="mt-2 flex flex-col gap-1">
                  {group.releases.map((candidate) => (
                    <ReleaseRow
                      key={candidate.version}
                      release={candidate}
                      selected={candidate.version === version}
                      onChoose={setVersion}
                    />
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1 text-callout text-fg-secondary">
            <span aria-hidden="true">{t("tool.version")}</span>
            <Select
              label={t("tool.version")}
              value={version}
              onValueChange={setVersion}
              options={list.map((candidate) => ({
                value: candidate.version,
                label: candidate.version,
              }))}
            />
          </div>
          {choice === null ? null : (
            <div className="flex flex-col gap-1 text-callout text-fg-secondary">
              <span aria-hidden="true">{t("tool.variant")}</span>
              <Select
                label={t("tool.variant")}
                value={chosenVariant ?? ""}
                onValueChange={setVariant}
                options={choice.values.map((value) => ({
                  value,
                  label: choice.labels[value] ?? value,
                }))}
              />
            </div>
          )}
          <span
            data-disabled-reason={reason}
            // The wrapper takes focus so keyboard users reach the reason of a disabled button.
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- same pattern as ActionButton
            tabIndex={reason === undefined ? undefined : 0}
            title={reason}
            className={`inline-flex rounded-control ${FOCUS_RING}`}
          >
            <Button
              variant="primary"
              disabled={reason !== undefined || request === null}
              loading={runner.starting}
              loadingLabel={t("tool.starting")}
              onClick={() => {
                if (request !== null) runner.start(request);
              }}
            >
              {t("tool.install")}
            </Button>
          </span>
        </div>
        {choice === null ? null : (
          <p className="text-caption text-fg-tertiary">
            {t("tool.variantHint", { key: choice.key })}
          </p>
        )}
        {reason === undefined ? null : <p className="text-caption text-fg-tertiary">{reason}</p>}
        <CliCommand command={cli} />
        {spec === null ? null : <PlanSummaryView query={plan} idle={!plan.isFetching} />}
      </>
    );
  }

  return (
    <section aria-labelledby="catalog-versions" className="flex flex-col gap-3">
      <h2 id="catalog-versions" className="text-title-2">
        {t("tool.versions")}
      </h2>
      {body}
    </section>
  );
}

// --- stack -------------------------------------------------------------------------------

function StackSection({ info, runner }: { info: ToolInfoDto; runner: InstallRunner }) {
  const { t } = useTranslation("catalog");
  const format = useFormatters();
  const id = info.tool.id;
  const plan = useJobQuery("tools.plan", { stack: id });
  const planCapability = useCapability("tools.plan");
  const planReason = useCapabilityReason(planCapability);
  const installReason = useCapabilityReason(useCapability("tools.install"));
  const summary = plan.data === undefined ? null : summarizePlan(plan.data);
  const specs = plan.data === undefined ? [] : planSpecs(plan.data);
  const nothing = summary !== null && summary.download.length === 0;
  const total =
    summary === null
      ? ""
      : `${summary.approximate ? t("plan.approx") : ""}${format.bytes(summary.totalBytes)}`;
  const reason = installReason ?? (summary === null ? t("stack.waitPlan") : undefined);
  return (
    <>
      <section aria-labelledby="catalog-stack" className="flex flex-col gap-3">
        <h2 id="catalog-stack" className="text-title-2">
          {t("stack.contains")}
        </h2>
        <ul className="flex flex-col gap-1 text-callout">
          {(info.stack?.tools ?? []).map((item) => (
            <li key={item.id} data-stack-tool={item.id}>
              {t("stack.tool", {
                tool: item.id,
                series: item.version ?? item.series ?? t("stack.latest"),
              })}
              {item.extensions !== undefined && item.extensions.length > 0
                ? ` + ${item.extensions.join(", ")}`
                : ""}
            </li>
          ))}
        </ul>
      </section>
      <section aria-labelledby="catalog-stack-install" className="flex flex-col gap-3">
        <h2 id="catalog-stack-install" className="text-title-2">
          {t("stack.install")}
        </h2>
        {planReason !== null && plan.data === undefined ? (
          <p className="text-callout text-fg-secondary">{planReason}</p>
        ) : (
          <PlanSummaryView query={plan} minHeight={STACK_PLAN_HEIGHT} />
        )}
        <div className="flex flex-wrap items-center gap-2">
          <span
            data-disabled-reason={reason}
            // The wrapper takes focus so keyboard users reach the reason of a disabled button.
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- same pattern as ActionButton
            tabIndex={reason === undefined ? undefined : 0}
            title={reason}
            className={`inline-flex rounded-control ${FOCUS_RING}`}
          >
            <Button
              variant="primary"
              disabled={reason !== undefined || nothing}
              loading={runner.starting}
              loadingLabel={t("tool.starting")}
              onClick={() => {
                runner.start({ specs, variant: null, label: info.tool.name });
              }}
            >
              {summary === null || nothing
                ? t("stack.confirmIdle")
                : t("stack.confirm", { size: total })}
            </Button>
          </span>
          <span
            title={t("stack.projectDisabled")}
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- the reason must be reachable by keyboard
            tabIndex={0}
            className={`inline-flex rounded-control ${FOCUS_RING}`}
          >
            <Button variant="secondary" disabled>
              {t("stack.createProject")}
            </Button>
          </span>
        </div>
        <p className="text-caption text-fg-tertiary">{t("stack.projectDisabled")}</p>
        {nothing ? (
          <p className="text-callout text-fg-secondary">{t("stack.allInstalled")}</p>
        ) : null}
        {specs.length > 0 ? <CliCommand command={installTools.cli({ specs })} /> : null}
      </section>
    </>
  );
}

// --- the page ----------------------------------------------------------------------------

const infoContext = (tool: string): ErrorContext => ({ method: "tools.info", params: { tool } });

/** /catalog/:id (docs/07 section 5.9): a tool or a stack, with versions, install and progress. */
export function ToolPage({ id }: { id: string }) {
  const { t } = useTranslation("catalog");
  const { t: tc } = useTranslation("common");
  const info = useForgeQuery("tools.info", { tool: id });
  const runtime = useRuntime();
  const status = useConnectionStatus();
  // A disabled query stays pending: a daemon without `tools.info`, or one that is away, must
  // explain itself instead of showing a skeleton for ever (R-16).
  const unavailable = useCapability("tools.info").reason === "not_implemented";
  const away = status === "stopped" || status === "closed" || status === "uninitialized";
  // The entry carries the icon hash and the registry/native badges `tools.info` does not.
  const everything = useForgeQuery("tools.search", { text: "", category: null });
  const entry = everything.data?.entries.find((candidate) => candidate.id === id);
  const runner = useInstallRunner();
  const dto = info.data;
  const isStack = dto?.kind === "stack";

  let body;
  if (info.isError) {
    body = (
      <ErrorNotice
        error={info.error}
        onRetry={() => {
          void info.refetch();
        }}
      />
    );
  } else if (unavailable && dto === undefined) {
    body = (
      <ErrorNotice
        error={NOT_IMPLEMENTED_ERROR}
        context={infoContext(id)}
        onRetry={() => {
          void info.refetch();
        }}
      />
    );
  } else if (away && dto === undefined) {
    body = (
      <EmptyState
        title={t("offline.title")}
        description={t("offline.description")}
        action={
          <Button
            onClick={() => {
              void runtime.api.start();
            }}
          >
            {tc("ui.asyncRetry")}
          </Button>
        }
      />
    );
  } else if (dto === undefined) {
    body = (
      <div aria-busy="true" data-testid="tool-loading">
        <span className="sr-only">{t("loading")}</span>
        <Skeleton height={160} />
      </div>
    );
  } else {
    const latest = entry === undefined ? dto.tool.latest : latestOf(entry);
    const homepage = safeHttpUrl(dto.homepage);
    const changelog = safeHttpUrl(dto.changelog_url);
    const docs = safeHttpUrl(dto.docs);
    body = (
      <>
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-1.5">
            {entry === undefined ? null : <EntryBadges entry={entry} />}
            {isStack ? <Badge tone="info">{t("badge.stack")}</Badge> : null}
            {dto.license === undefined || dto.license === null ? null : (
              <Badge>{dto.license}</Badge>
            )}
            {latest === null ? null : (
              <span className={`text-caption text-fg-secondary ${TNUM_CLASS}`}>
                {t("card.latest", { version: latest })}
              </span>
            )}
          </div>
          <div className="flex flex-wrap gap-3 text-callout">
            {changelog === null ? null : (
              <ExternalAnchor url={changelog}>{t("tool.changelog")}</ExternalAnchor>
            )}
            {homepage === null ? null : (
              <ExternalAnchor url={homepage}>{t("tool.homepage")}</ExternalAnchor>
            )}
            {docs === null ? null : <ExternalAnchor url={docs}>{t("tool.docs")}</ExternalAnchor>}
          </div>
        </div>
        {dto.description_md === undefined || dto.description_md === null ? (
          dto.tool.description === null ? null : (
            <p className="text-body">{dto.tool.description}</p>
          )
        ) : (
          <SafeMarkdown source={dto.description_md} />
        )}
        <InstallProgressList
          items={runner.items}
          onRetry={runner.retry}
          onDismiss={runner.dismiss}
        />
        {runner.failure === null ? null : <ErrorNotice error={runner.failure} />}
        {isStack ? (
          <StackSection info={dto} runner={runner} />
        ) : (
          <>
            {/* Installed first: it is known with `tools.info`; the async releases come last, so
                nothing below them shifts when they arrive. */}
            <InstalledSection info={dto} />
            <InstallSection info={dto} runner={runner} />
          </>
        )}
      </>
    );
  }

  return (
    <div
      data-testid="page-catalog"
      data-page="tool"
      className="flex min-h-0 flex-1 flex-col overflow-y-auto"
    >
      <div className="flex flex-col gap-5 p-4">
        <Link to={CATALOG_ROUTE} className={`w-fit text-callout ${LINK_CLASS}`}>
          {t("tool.back")}
        </Link>
        <header className="flex items-center gap-4">
          <CatalogIcon
            tool={id}
            sha256={entry?.icon_sha256}
            color={entry?.icon_color}
            variant="hero"
          />
          <h1 className="min-w-0 flex-1 truncate text-title-1">
            {dto?.tool.name ?? entry?.name ?? id}
          </h1>
        </header>
        {body}
      </div>
    </div>
  );
}
