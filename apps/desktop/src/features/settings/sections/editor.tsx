import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../../actions/registry";
import { testTerminal } from "../../../actions/system";
import { Button } from "../../../design/primitives/Button";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { toForgeApiError, type ForgeApiError } from "../../../ipc/errors";
import { useCapability, useForgeApi, useRuntime } from "../../../state";
import { loose, useRoot } from "../looseT";
import { matchesSearch, PRESETS_BY_KEY, valueAt } from "../model";
import type { SectionProps } from "../registry";
import { editorRows, rowDescription, rowLabel } from "../schemaRows";
import { SettingRow } from "../SettingRow";

type TestKind = "editor" | "terminal";

/**
 * "Testar": runs the SAVED command on the daemon's machine (`shell.open_editor` on the root,
 * `shell.open` in it, through the `settings.terminal_test` action so it is in the GUI/CLI parity
 * gate as `forge shell <dir>`). Not rendered on a remote context (both are `local_only`).
 */
function TestButton({ kind, dirty }: { kind: TestKind; dirty: boolean }) {
  const { t } = useTranslation("settings");
  const api = useForgeApi();
  const runtime = useRuntime();
  const method = kind === "editor" ? "shell.open_editor" : "shell.open";
  const capability = useCapability(method);
  const root = useRoot();
  const [phase, setPhase] = useState<"idle" | "running" | "ok">("idle");
  const [error, setError] = useState<ForgeApiError | null>(null);
  if (capability.reason === "local_only") return null;

  const run = () => {
    setPhase("running");
    setError(null);
    const call =
      kind === "editor"
        ? api.call("shell.open_editor", { path: root })
        : runAction(runtime.queue, testTerminal, { dir: root });
    call.then(
      () => {
        setPhase("ok");
      },
      (failure: unknown) => {
        setPhase("idle");
        setError(toForgeApiError(failure, { method }));
      },
    );
  };

  return (
    <div className="flex flex-col gap-1">
      <Button
        data-testid={`test-${kind}`}
        loading={phase === "running"}
        loadingLabel={t("test.running")}
        disabled={dirty || !capability.available || root === ""}
        onClick={run}
      >
        {loose(t)(`test.${kind}`)}
      </Button>
      {dirty ? <span className="text-caption text-fg-tertiary">{t("test.saveFirst")}</span> : null}
      {phase === "ok" ? (
        <span data-testid={`test-${kind}-ok`} className="text-caption text-fg-secondary">
          {t("test.sent")}
        </span>
      ) : null}
      {error === null ? null : <ErrorNotice error={error} context={{ method }} />}
    </div>
  );
}

/**
 * Editor/Terminal: `editor.command`, `terminal.command`, `terminal.integrated_shell` and
 * `terminal.max_sessions`, built from the schema with presets and a "Testar" button.
 */
export function EditorSection({ query, doc }: SectionProps) {
  const { t } = useTranslation("settings");
  const rows = useMemo(() => editorRows(doc), [doc]);
  const [dirty, setDirty] = useState<Readonly<Record<string, boolean>>>({});
  const mark = useCallback((key: string, value: boolean) => {
    setDirty((current) => (current[key] === value ? current : { ...current, [key]: value }));
  }, []);
  const handlers = useMemo(
    () =>
      Object.fromEntries(
        rows.map((row) => [
          row.key,
          (value: boolean) => {
            mark(row.key, value);
          },
        ]),
      ),
    [rows, mark],
  );

  if (doc === undefined) return <Skeleton height={96} />;
  const presetLabel = (id: string) => loose(t)(`presets.${id}`);

  return (
    <div className="flex flex-col">
      {rows
        .filter((row) => matchesSearch(query, rowLabel(t, row), row.key))
        .map((row) => {
          const test: TestKind | null =
            row.key === "editor.command"
              ? "editor"
              : row.key === "terminal.command"
                ? "terminal"
                : null;
          const presets = PRESETS_BY_KEY[row.key];
          return (
            <SettingRow
              key={row.key}
              spec={row}
              label={rowLabel(t, row)}
              description={rowDescription(t, row)}
              value={valueAt(doc.values, row.key)}
              {...(presets !== undefined && { presets, presetLabel })}
              extra={
                test === null ? null : <TestButton kind={test} dirty={dirty[row.key] === true} />
              }
              onDirtyChange={handlers[row.key] ?? (() => undefined)}
            />
          );
        })}
    </div>
  );
}
