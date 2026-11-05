import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Select } from "../../design/primitives/Select";
import { cx } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import type { CatalogEntryDto, ToolPlanItemDto } from "../../ipc/generated";
import { useForgeQuery } from "../../state";
import { CatalogIcon } from "../catalog/CatalogIcon";
import { useJobQuery } from "../catalog/hooks";
import { groupBySeries } from "../catalog/model";
import { PlanSummaryView } from "../catalog/PlanSummaryView";
import { EMPTY_STACK, ONBOARDING_TAG, planParams } from "./model";
import { StepNav } from "./StepNav";
import type { Wizard } from "./useWizard";

/** Room for the plan of a stack of four items, so the buttons below do not jump. */
const PLAN_HEIGHT = 190;

/** The onboarding cards: stacks tagged `onboarding`, in the order the index lists them. */
export function onboardingStacks(entries: readonly CatalogEntryDto[]): CatalogEntryDto[] {
  return entries.filter((entry) => entry.kind === "stack" && entry.tags.includes(ONBOARDING_TAG));
}

interface CardProps {
  id: string;
  name: string;
  summary: string | null;
  icon: React.ReactNode;
  checked: boolean;
  onSelect: () => void;
}

function StackCard({ id, name, summary, icon, checked, onSelect }: CardProps) {
  return (
    <label
      data-stack-card={id}
      data-checked={checked}
      className={cx(
        "flex cursor-pointer items-start gap-3 rounded-card border bg-surface p-3 has-focus-visible:outline-solid has-focus-visible:outline-2 has-focus-visible:outline-offset-2 has-focus-visible:outline-accent",
        checked ? "border-accent" : "border-stroke",
      )}
    >
      <input
        type="radio"
        name="onboarding-stack"
        value={id}
        checked={checked}
        onChange={onSelect}
        className="sr-only"
      />
      <span className="mt-0.5 shrink-0">{icon}</span>
      <span className="flex min-w-0 flex-col gap-1">
        <span className="text-headline">{name}</span>
        {summary === null ? null : (
          <span className="text-callout text-fg-secondary">{summary}</span>
        )}
      </span>
    </label>
  );
}

/** Series of the tool at `version`, falling back to the series in the plan item's spec. */
function currentSeries(item: ToolPlanItemDto, seriesOf: ReadonlyMap<string, string>): string {
  return seriesOf.get(item.version) ?? item.spec.split("@")[1] ?? "";
}

function VersionPicker({
  item,
  value,
  onChange,
}: {
  item: ToolPlanItemDto;
  value: string | undefined;
  onChange: (series: string) => void;
}) {
  const { t } = useTranslation("onboarding");
  const releases = useJobQuery("tools.releases", { tool: item.tool });
  const groups = useMemo(() => groupBySeries(releases.data?.releases ?? []), [releases.data]);
  if (releases.isError) {
    return (
      <div className="flex flex-col gap-1">
        <p className="text-callout text-fg-secondary">
          {t("stack.versionsFailed", { tool: item.tool })}
        </p>
        <ErrorNotice
          error={releases.error}
          onRetry={() => {
            void releases.refetch();
          }}
        />
      </div>
    );
  }
  if (releases.data === undefined) {
    return (
      <div aria-busy="true">
        <span className="sr-only">{t("stack.versionsLoading")}</span>
        <Skeleton height={32} />
      </div>
    );
  }
  const seriesOf = new Map(releases.data.releases.map((r) => [r.version, r.series]));
  const selected = value ?? currentSeries(item, seriesOf);
  return (
    <div className="flex flex-col gap-1">
      <Select
        label={t("stack.versionLabel", { tool: item.tool })}
        value={selected}
        options={groups.map((group) => ({ value: group.series, label: group.series }))}
        onValueChange={onChange}
      />
      {releases.data.stale === true ? (
        <p className="text-caption text-fg-tertiary">{t("stack.versionsStale")}</p>
      ) : null}
    </div>
  );
}

/**
 * Step 2: the stack cards (`tools.search`, tag `onboarding`, index order, plus the built-in
 * "Vazio"), the `tools.plan` download list with its total and, on demand, the versions of each
 * tool (`tools.releases`); a changed version plans again.
 */
export function StackStep({
  wizard,
  onBack,
  onNext,
}: {
  wizard: Wizard;
  onBack: (() => void) | null;
  onNext: () => void;
}) {
  const { t } = useTranslation("onboarding");
  const { choices, update } = wizard;
  const search = useForgeQuery("tools.search", {});
  const [adjusting, setAdjusting] = useState(false);
  const stacks = useMemo(() => onboardingStacks(search.data?.entries ?? []), [search.data]);
  const real = choices.stack !== null && choices.stack !== EMPTY_STACK;
  const params = planParams(choices.stack ?? "", choices.versions);
  const plan = useJobQuery("tools.plan", params, { enabled: real });
  const items = (plan.data?.items ?? []).filter((item) => item.kind !== "extension");

  const select = (id: string) => {
    if (id !== choices.stack) update({ stack: id, versions: {} });
  };

  return (
    <div className="flex flex-col gap-4">
      <fieldset className="flex flex-col gap-2">
        <legend className="sr-only">{t("stack.groupLabel")}</legend>
        {search.isError ? (
          <ErrorNotice
            error={search.error}
            onRetry={() => {
              void search.refetch();
            }}
          />
        ) : null}
        {search.isLoading ? (
          <div aria-busy="true">
            <span className="sr-only">{t("stack.loadingCards")}</span>
            <Skeleton height={64} />
          </div>
        ) : null}
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
          {stacks.map((entry) => (
            <StackCard
              key={entry.id}
              id={entry.id}
              name={entry.name}
              summary={entry.summary ?? entry.description}
              icon={
                <CatalogIcon tool={entry.id} sha256={entry.icon_sha256} color={entry.icon_color} />
              }
              checked={choices.stack === entry.id}
              onSelect={() => {
                select(entry.id);
              }}
            />
          ))}
          <StackCard
            id={EMPTY_STACK}
            name={t("stack.emptyName")}
            summary={t("stack.emptyDescription")}
            icon={
              <span
                aria-hidden="true"
                className="block size-10 rounded-card border border-stroke"
              />
            }
            checked={choices.stack === EMPTY_STACK}
            onSelect={() => {
              select(EMPTY_STACK);
            }}
          />
        </div>
        {!search.isLoading && !search.isError && stacks.length === 0 ? (
          <p className="text-callout text-fg-secondary">{t("stack.noStacks")}</p>
        ) : null}
      </fieldset>

      {real ? (
        <section className="flex flex-col gap-3" aria-label={t("stack.planTitle")}>
          <PlanSummaryView query={plan} minHeight={PLAN_HEIGHT} />
          {plan.data !== undefined && items.length > 0 ? (
            <div className="flex flex-col gap-2">
              <div>
                <Button
                  variant="ghost"
                  aria-expanded={adjusting}
                  onClick={() => {
                    setAdjusting(!adjusting);
                  }}
                >
                  {adjusting ? t("stack.adjustHide") : t("stack.adjust")}
                </Button>
              </div>
              {adjusting
                ? items.map((item) => (
                    <VersionPicker
                      key={item.tool}
                      item={item}
                      value={choices.versions[item.tool]}
                      onChange={(series) => {
                        update({ versions: { ...choices.versions, [item.tool]: series } });
                      }}
                    />
                  ))
                : null}
            </div>
          ) : null}
        </section>
      ) : null}
      <StepNav
        onBack={onBack}
        onNext={onNext}
        nextDisabled={choices.stack === null || (real && plan.data === undefined)}
      />
    </div>
  );
}
