import { useTranslation } from "react-i18next";
import { disableCaseDir, enableCaseDir } from "../../../../actions/filesystem";
import { ActionButton } from "../../../../design/primitives/ActionButton";
import { Badge, type BadgeTone } from "../../../../design/primitives/Badge";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { renderCli } from "../../../../ipc/cli";
import { useForgeQuery } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { CASE_DIRS, caseState, joinPath, type CaseState } from "./model";

const METHOD_STATUS = "fs_parity.case_status";
const DASH = "—";

const TONE: Record<CaseState, BadgeTone> = {
  enabled: "success",
  disabled: "neutral",
  unsupported: "warning",
};

function CaseRow({ root, name }: { root: string; name: string }) {
  const { t } = useTranslation("settings");
  const dir = joinPath(root, name);
  const query = useForgeQuery(METHOD_STATUS, { dir });
  const status = query.data;
  const state = status === undefined ? null : caseState(status);
  return (
    <tr data-testid={`case-row-${name}`} data-state={state ?? "loading"} className="align-top">
      <td className="py-2 pr-3 font-mono text-mono">{name}/</td>
      <td className="py-2 pr-3">
        {query.isError ? (
          <ErrorNotice
            error={query.error}
            context={{ method: METHOD_STATUS, params: { dir } }}
            onRetry={() => {
              void query.refetch();
            }}
          />
        ) : state === null ? (
          <span className="text-fg-tertiary">{DASH}</span>
        ) : (
          <div className="flex flex-col gap-1">
            <Badge tone={TONE[state]}>{loose(t)(`filesystem.table.states.${state}`)}</Badge>
            {status?.reason == null ? null : (
              <span data-testid={`case-reason-${name}`} className="text-caption text-fg-secondary">
                {status.reason}
              </span>
            )}
          </div>
        )}
      </td>
      <td className="py-2 pr-3 text-callout text-fg-secondary">{status?.fs_type ?? DASH}</td>
      <td className="py-2">
        {state === null || state === "unsupported" ? null : state === "enabled" ? (
          <ActionButton action={disableCaseDir} input={dir}>
            {t("filesystem.table.disable")}
          </ActionButton>
        ) : (
          <ActionButton action={enableCaseDir} input={dir}>
            {t("filesystem.table.enable")}
          </ActionButton>
        )}
      </td>
    </tr>
  );
}

/** Per-directory status (`fs_parity.case_status`) of the folders the Forgebox creates. */
export function CaseTable({ root }: { root: string }) {
  const { t } = useTranslation("settings");
  return (
    <RowFrame
      id="fs-table"
      label={t("filesystem.table.label")}
      description={t("filesystem.table.description")}
    >
      <div className="flex w-full min-w-96 flex-col gap-2">
        {root === "" ? (
          <p className="text-callout text-fg-secondary">{t("filesystem.table.noRoot")}</p>
        ) : (
          <table className="w-full text-left text-callout">
            <thead>
              <tr className="text-caption text-fg-tertiary">
                <th className="pr-3 font-normal">{t("filesystem.table.dir")}</th>
                <th className="pr-3 font-normal">{t("filesystem.table.state")}</th>
                <th className="pr-3 font-normal">{t("filesystem.table.fs")}</th>
                <th className="font-normal">
                  <span className="sr-only">{t("filesystem.table.actions")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {CASE_DIRS.map((name) => (
                <CaseRow key={name} root={root} name={name} />
              ))}
            </tbody>
          </table>
        )}
        <CliCommand command={renderCli(METHOD_STATUS, { dir: joinPath(root, CASE_DIRS[0]) })} />
      </div>
    </RowFrame>
  );
}
