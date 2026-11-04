import { useState } from "react";
import { useTranslation } from "react-i18next";
import { checkAllUpdates } from "../../../../actions/filesystem";
import { Button } from "../../../../design/primitives/Button";
import { Select } from "../../../../design/primitives/Select";
import type { ToolUpdatesDto } from "../../../../ipc/generated";
import { UpdaterPanel, useUpdaterSnapshot } from "../../../updater";
import { loose } from "../../looseT";
import { buildRows, matchesSearch, valueAt, withKnownOptions } from "../../model";
import type { SectionProps } from "../../registry";
import { RowFrame } from "../../RowFrame";
import { SettingRow } from "../../SettingRow";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";
import { PolicyTable } from "./PolicyTable";
import { QuietHours } from "./QuietHours";

const TID_UPDATE_APP_INACTIVE = "update-app-inactive";
const TID_UPDATE_CHECK_NOW = "update-check-now";
const TID_UPDATE_CHECK_STATUS = "update-check-status";
const TID_UPDATE_CHECK_RESULT = "update-check-result";
const CHANNEL_STABLE = "stable";
const CHANNEL_BETA = "beta";

/** Row ids of the section (`updates.<id>.label` in the `settings` namespace). */
export const UPDATES_ROWS = ["app", "check", "quiet", "metered", "policy"] as const;

const KEY_METERED = "updates.metered";
const METHOD_CHECK = "tools.check_updates";

/**
 * App channel and self-update. While the updater is inactive (the 0.2.0 release build has no
 * production key or endpoint) the controls are shown disabled with the reason; an active updater
 * (dev builds, tests) renders the full panel.
 */
function AppUpdates() {
  const { t } = useTranslation("settings");
  const snapshot = useUpdaterSnapshot();
  return (
    <RowFrame
      id="update-app"
      label={t("updates.app.label")}
      description={t("updates.app.description")}
    >
      {snapshot.active ? (
        <div className="-mx-4 w-full">
          <UpdaterPanel />
        </div>
      ) : (
        <div className="flex flex-col gap-2" data-testid={TID_UPDATE_APP_INACTIVE}>
          <div className="flex flex-wrap items-center gap-2">
            <Select
              label={t("updates.app.channel")}
              value={CHANNEL_STABLE}
              disabled
              width="sm"
              options={[
                { value: CHANNEL_STABLE, label: t("updates.app.stable") },
                { value: CHANNEL_BETA, label: t("updates.app.beta") },
              ]}
            />
            <Button disabled>{t("updates.app.selfUpdate")}</Button>
          </div>
          <p className="text-callout text-fg-secondary">{t("updates.app.inactive")}</p>
        </div>
      )}
    </RowFrame>
  );
}

/** Atualizacoes (docs/05 section 4): app updater, check now, quiet hours, metered policy, tools. */
export function UpdatesSection({ query, doc }: SectionProps) {
  const { t } = useTranslation("settings");
  const [found, setFound] = useState<number | null>(null);
  const check = useJobAction(checkAllUpdates, (result) => {
    setFound((result as ToolUpdatesDto).updates.length);
  });
  const show = (id: (typeof UPDATES_ROWS)[number]) =>
    matchesSearch(query, loose(t)(`updates.${id}.label`));
  const metered =
    doc === undefined ? undefined : buildRows(doc).find((row) => row.key === KEY_METERED);
  return (
    <div className="flex flex-col">
      {show("app") ? <AppUpdates /> : null}
      {show("check") ? (
        <RowFrame
          id="update-check"
          label={t("updates.check.label")}
          description={t("updates.check.description")}
        >
          <div className="flex flex-col gap-2">
            <div>
              <JobButton
                action={checkAllUpdates}
                input={undefined}
                job={check}
                variant="primary"
                testId={TID_UPDATE_CHECK_NOW}
                onPress={() => {
                  setFound(null);
                  check.run(undefined);
                }}
              >
                {t("updates.check.now")}
              </JobButton>
            </div>
            <JobStatus
              job={check}
              label={t("updates.check.checking")}
              method={METHOD_CHECK}
              testId={TID_UPDATE_CHECK_STATUS}
            />
            {found === null || check.phase !== "done" ? null : (
              <p data-testid={TID_UPDATE_CHECK_RESULT} role="status" className="text-callout">
                {found === 0 ? t("updates.check.none") : t("updates.check.found", { count: found })}
              </p>
            )}
          </div>
        </RowFrame>
      ) : null}
      {show("quiet") && doc !== undefined ? <QuietHours values={doc.values} /> : null}
      {show("metered") && doc !== undefined && metered !== undefined ? (
        <SettingRow
          spec={withKnownOptions(metered)}
          label={t("updates.metered.label")}
          description={t("updates.metered.description")}
          value={valueAt(doc.values, KEY_METERED)}
          optionLabel={(option) => loose(t)(`updates.metered.values.${option}`)}
        />
      ) : null}
      {show("policy") ? <PolicyTable /> : null}
    </div>
  );
}
