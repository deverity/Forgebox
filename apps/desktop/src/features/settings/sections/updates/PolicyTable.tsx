import { useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../../../actions/registry";
import { setToolPolicy } from "../../../../actions/tools";
import { Badge } from "../../../../design/primitives/Badge";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { EmptyState } from "../../../../design/primitives/EmptyState";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { Select } from "../../../../design/primitives/Select";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { ToolDto, UpdatePolicy } from "../../../../ipc/generated";
import { useCapability, useForgeQuery, useRuntime } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { UPDATE_POLICIES } from "./model";

const METHOD_LIST = "tools.list";
const METHOD_POLICY = "tools.policy";

function PolicyRow({ tool }: { tool: ToolDto }) {
  const { t } = useTranslation("settings");
  const runtime = useRuntime();
  const capability = useCapability(METHOD_POLICY);
  // The value just chosen, shown until `tools.list` brings the saved one.
  const [chosen, setChosen] = useState<UpdatePolicy | null>(null);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const policy = chosen ?? tool.policy;
  const command = setToolPolicy.cli({ tool: tool.id, policy });
  const change = (next: UpdatePolicy) => {
    setChosen(next);
    setFailure(null);
    runAction(
      runtime.queue,
      setToolPolicy,
      { tool: tool.id, policy: next },
      { intent: newIntent() },
    ).then(
      () => undefined,
      (error: unknown) => {
        setChosen(null);
        setFailure(toForgeApiError(error, { method: METHOD_POLICY }));
      },
    );
  };
  return (
    <tr data-testid={`policy-${tool.id}`} className="align-top">
      <td className="py-2 pr-3">
        <div className="flex flex-col">
          <span className="text-body">{tool.name}</span>
          <span className="font-mono text-caption text-fg-tertiary">{tool.id}</span>
        </div>
      </td>
      <td className="py-2 pr-3 text-callout text-fg-secondary">
        {tool.installed.length}
        {tool.update_available === null ? null : (
          <>
            {" "}
            <Badge tone="info">
              {t("updates.policy.available", { version: tool.update_available })}
            </Badge>
          </>
        )}
      </td>
      <td className="py-2">
        <div className="flex flex-col gap-1" data-forge-cli={command ?? undefined}>
          <Select
            label={t("updates.policy.select", { tool: tool.name })}
            value={policy}
            disabled={!capability.available}
            options={UPDATE_POLICIES.map((value) => ({
              value,
              label: loose(t)(`updates.policy.values.${value}`),
            }))}
            onValueChange={(value) => {
              change(value as UpdatePolicy);
            }}
          />
          <CliCommand command={command} />
          {failure === null ? null : (
            <ErrorNotice error={failure} context={{ method: METHOD_POLICY }} />
          )}
        </div>
      </td>
    </tr>
  );
}

/** Per-tool update policy (`tools.policy`), one row for every tool with an installed version. */
export function PolicyTable() {
  const { t } = useTranslation("settings");
  const list = useForgeQuery(METHOD_LIST, { installed: true });
  const tools = list.data?.tools ?? [];
  return (
    <RowFrame
      id="update-policy"
      label={t("updates.policy.label")}
      description={t("updates.policy.description")}
    >
      <div className="flex w-full min-w-96 flex-col gap-2">
        {list.isError ? (
          <ErrorNotice
            error={list.error}
            context={{ method: METHOD_LIST }}
            onRetry={() => {
              void list.refetch();
            }}
          />
        ) : tools.length === 0 && !list.isPending ? (
          <EmptyState title={t("updates.policy.empty")} />
        ) : (
          <table className="w-full text-left text-callout">
            <thead>
              <tr className="text-caption text-fg-tertiary">
                <th className="pr-3 font-normal">{t("updates.policy.tool")}</th>
                <th className="pr-3 font-normal">{t("updates.policy.installed")}</th>
                <th className="font-normal">{t("updates.policy.policy")}</th>
              </tr>
            </thead>
            <tbody>
              {tools.map((tool) => (
                <PolicyRow key={tool.id} tool={tool} />
              ))}
            </tbody>
          </table>
        )}
      </div>
    </RowFrame>
  );
}
