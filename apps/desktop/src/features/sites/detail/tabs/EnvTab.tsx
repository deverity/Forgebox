import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useCapabilityReason } from "../../../../app/shell/useErrorText";
import { Button } from "../../../../design/primitives/Button";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { EmptyState } from "../../../../design/primitives/EmptyState";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { Skeleton } from "../../../../design/primitives/Skeleton";
import { renderCli } from "../../../../ipc/cli";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type * as T from "../../../../ipc/generated";
import { copyText } from "../../../../ipc/native";
import { useCapability, useForgeApi, useForgeQuery, useHello, useSite } from "../../../../state";

/** A revealed secret hides itself after this long (docs/11 section 6). */
export const REVEAL_MS = 30_000;
const ENV_METHOD = "sites.env";
const MASK = "••••••••";

/**
 * Revealed values live ONLY here, in component state: never in zustand, never in the TanStack
 * cache (the reveal call goes through the api, not `useForgeQuery`), never logged.
 * Keys are `file:line`.
 */
function useReveals(siteId: string) {
  const api = useForgeApi();
  const [values, setValues] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const alive = useRef(true);

  const hide = useCallback((id: string) => {
    const timer = timers.current.get(id);
    if (timer !== undefined) clearTimeout(timer);
    timers.current.delete(id);
    setValues((current) => Object.fromEntries(Object.entries(current).filter(([k]) => k !== id)));
  }, []);

  useEffect(() => {
    alive.current = true;
    const pending = timers.current;
    return () => {
      alive.current = false;
      pending.forEach((timer) => {
        clearTimeout(timer);
      });
      pending.clear();
    };
  }, []);

  const reveal = useCallback(
    async (file: string, variable: T.DotenvVarDto) => {
      const id = `${file}:${String(variable.line)}`;
      setBusy(id);
      setFailure(null);
      try {
        const result = await api.call("sites.env", { site: siteId, reveal: true });
        if (!alive.current) return;
        // Only the asked value is kept; the rest of the answer is dropped.
        const found = result.files
          .find((f) => f.name === file)
          ?.vars.find((v) => v.line === variable.line && v.key === variable.key);
        const value = found?.value ?? null;
        if (value === null) return;
        setValues((current) => ({ ...current, [id]: value }));
        const previous = timers.current.get(id);
        if (previous !== undefined) clearTimeout(previous);
        timers.current.set(
          id,
          setTimeout(() => {
            hide(id);
          }, REVEAL_MS),
        );
      } catch (error) {
        if (alive.current) setFailure(toForgeApiError(error));
      } finally {
        if (alive.current) setBusy(null);
      }
    },
    [api, siteId, hide],
  );

  return { values, failure, busy, reveal, hide };
}

interface RowProps {
  variable: T.DotenvVarDto;
  revealed: string | undefined;
  busy: boolean;
  canReveal: boolean;
  revealReason: string | null;
  onReveal: () => void;
  onHide: () => void;
}

function VarRow({ variable, revealed, busy, canReveal, revealReason, onReveal, onHide }: RowProps) {
  const { t } = useTranslation("sites");
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  const visible = variable.secret ? revealed : (variable.value ?? undefined);
  const copyValue = () => {
    if (visible === undefined) return;
    void copyText(visible).then((ok) => {
      setCopy(ok ? "copied" : "failed");
    });
  };
  return (
    <tr data-testid="env-row" data-secret={variable.secret} className="border-t border-stroke">
      <td className="px-2 py-1 font-mono text-mono">{variable.key}</td>
      <td className="max-w-md px-2 py-1 font-mono text-mono">
        {visible === undefined ? (
          <span data-testid="env-masked" aria-label={t("detail.env.masked")}>
            {MASK}
          </span>
        ) : (
          <span data-testid="env-value" className="break-all">
            {visible}
          </span>
        )}
      </td>
      <td className="flex gap-1 px-2 py-1">
        {variable.secret && revealed === undefined ? (
          <span title={revealReason ?? undefined}>
            <Button
              variant="ghost"
              disabled={!canReveal || busy}
              aria-label={t("detail.env.revealKey", { key: variable.key })}
              onClick={onReveal}
            >
              {t("detail.env.reveal")}
            </Button>
          </span>
        ) : null}
        {variable.secret && revealed !== undefined ? (
          <Button
            variant="ghost"
            aria-label={t("detail.env.hideKey", { key: variable.key })}
            onClick={onHide}
          >
            {t("detail.env.hide")}
          </Button>
        ) : null}
        {visible !== undefined ? (
          <Button
            variant="ghost"
            aria-label={t("detail.env.copyKey", { key: variable.key })}
            onClick={copyValue}
          >
            {copy === "copied"
              ? t("detail.env.copied")
              : copy === "failed"
                ? t("detail.env.copyFailed")
                : t("detail.env.copy")}
          </Button>
        ) : null}
      </td>
    </tr>
  );
}

/** "Ambiente": the site's `.env` files with masked secrets (docs/07 section 5.4, docs/11 section 6). */
export function EnvTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("sites");
  const api = useForgeApi();
  const site = useSite(siteId);
  const hello = useHello();
  const query = useForgeQuery("sites.env", { site: siteId });
  const editor = useCapability("shell.open_editor");
  const editorReason = useCapabilityReason(editor);
  const { values, failure, busy, reveal, hide } = useReveals(siteId);
  const [editorFailure, setEditorFailure] = useState<ForgeApiError | null>(null);
  const isAdmin = hello?.role === "admin";
  const revealReason = isAdmin ? null : t("detail.env.adminOnly", { role: hello?.role ?? "-" });
  const host = site?.host ?? query.data?.host ?? "";

  const openEditor = (path: string) => {
    setEditorFailure(null);
    api.call("shell.open_editor", { path }).catch((error: unknown) => {
      setEditorFailure(toForgeApiError(error));
    });
  };

  if (query.isError) {
    return (
      <ErrorNotice
        error={query.error}
        context={{ method: ENV_METHOD, params: { site: siteId } }}
        onRetry={() => {
          void query.refetch();
        }}
      />
    );
  }
  if (query.data === undefined) {
    return (
      <div aria-busy="true" data-testid="env-loading">
        <span className="sr-only">{t("detail.env.loading")}</span>
        <Skeleton height={120} />
      </div>
    );
  }
  const files = query.data.files;
  return (
    <div data-testid="site-env-tab" className="flex max-w-4xl flex-col gap-4">
      <header className="flex flex-col gap-1">
        <h2 className="text-headline">{t("detail.env.title", { host })}</h2>
        <p className="text-callout text-fg-secondary">{t("detail.env.description")}</p>
      </header>
      {files.length === 0 ? (
        <EmptyState title={t("detail.env.empty")} description={t("detail.env.emptyHint")} />
      ) : (
        files.map((file) => (
          <section
            key={file.name}
            aria-label={file.name}
            data-testid="env-file"
            className="flex flex-col gap-2"
          >
            <div className="flex items-center justify-between gap-2">
              <h3 className="font-mono text-mono">{file.name}</h3>
              <span title={editorReason ?? undefined}>
                <Button
                  variant="ghost"
                  disabled={!editor.available}
                  onClick={() => {
                    openEditor(file.path);
                  }}
                >
                  {t("detail.env.openEditor")}
                </Button>
              </span>
            </div>
            {file.vars.length === 0 ? (
              <p className="text-callout text-fg-secondary">{t("detail.env.fileEmpty")}</p>
            ) : (
              <table className="w-full text-left text-callout">
                <thead className="sr-only">
                  <tr>
                    <th>{t("detail.env.key")}</th>
                    <th>{t("detail.env.value")}</th>
                    <th>{t("detail.env.actions")}</th>
                  </tr>
                </thead>
                <tbody>
                  {file.vars.map((variable) => {
                    const id = `${file.name}:${String(variable.line)}`;
                    return (
                      <VarRow
                        key={id}
                        variable={variable}
                        revealed={values[id]}
                        busy={busy === id}
                        canReveal={isAdmin}
                        revealReason={revealReason}
                        onReveal={() => {
                          void reveal(file.name, variable);
                        }}
                        onHide={() => {
                          hide(id);
                        }}
                      />
                    );
                  })}
                </tbody>
              </table>
            )}
          </section>
        ))
      )}
      {!isAdmin && files.some((f) => f.vars.some((v) => v.secret)) ? (
        <p role="note" data-testid="env-reveal-reason" className="text-callout text-fg-secondary">
          {revealReason}
        </p>
      ) : null}
      {Object.keys(values).length > 0 ? (
        <p role="status" className="text-callout text-fg-secondary">
          {t("detail.env.autoHide", { seconds: REVEAL_MS / 1000 })}
        </p>
      ) : null}
      {failure === null ? null : <ErrorNotice error={failure} />}
      {editorFailure === null ? null : <ErrorNotice error={editorFailure} />}
      <CliCommand command={renderCli("sites.env", { site: host })} />
    </div>
  );
}
