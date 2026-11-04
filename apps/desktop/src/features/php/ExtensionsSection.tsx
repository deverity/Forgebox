import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { disablePhpExtension, enablePhpExtension } from "../../actions/php";
import { runAction } from "../../actions/registry";
import { Badge } from "../../design/primitives/Badge";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Skeleton } from "../../design/primitives/Skeleton";
import { Toggle } from "../../design/primitives/Toggle";
import { Tooltip } from "../../design/primitives/Tooltip";
import { TNUM_CLASS } from "../../i18n/format";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import { useCapability, useForgeQuery, useRuntime } from "../../state";
import { filterExtensions, indexExtensions, shownEnabled } from "./model";

function without<V>(record: Readonly<Record<string, V>>, key: string): Record<string, V> {
  return Object.fromEntries(Object.entries(record).filter(([name]) => name !== key));
}

/**
 * Extensions of one series: search and a toggle each. A toggle is optimistic: it shows the new
 * state at once and goes back (with the cause) when the job fails (`php.ext_enable|disable`).
 */
export function ExtensionsSection({ series }: { series: string }) {
  const { t } = useTranslation("php");
  const runtime = useRuntime();
  const capability = useCapability(enablePhpExtension.method);
  const list = useForgeQuery("php.ext_list", { version: series });
  const [query, setQuery] = useState("");
  const [pending, setPending] = useState<Readonly<Record<string, boolean>>>({});
  const [failures, setFailures] = useState<Readonly<Record<string, ForgeApiError>>>({});

  const index = useMemo(() => indexExtensions(list.data?.extensions ?? []), [list.data]);
  const shown = useMemo(() => filterExtensions(index, query), [index, query]);

  const release = (name: string) => {
    setPending((current) => without(current, name));
  };

  const toggle = (name: string, next: boolean) => {
    setPending((current) => ({ ...current, [name]: next }));
    setFailures((current) => without(current, name));
    const action = next ? enablePhpExtension : disablePhpExtension;
    runAction(
      runtime.queue,
      action,
      { version: series, extension: name },
      { intent: newIntent() },
    ).then(
      // Keep the optimistic value until the refetched list agrees, so the switch never flickers.
      () =>
        list.refetch().then(
          () => {
            release(name);
          },
          () => {
            release(name);
          },
        ),
      (error: unknown) => {
        release(name);
        setFailures((current) => ({
          ...current,
          [name]: toForgeApiError(error, { method: action.method }),
        }));
      },
    );
  };

  let body;
  if (list.isError) {
    body = (
      <ErrorNotice
        error={list.error}
        onRetry={() => {
          void list.refetch();
        }}
      />
    );
  } else if (list.data === undefined) {
    body = <Skeleton height={120} />;
  } else if (index.length === 0) {
    body = <p className="text-callout text-fg-secondary">{t("ext.empty")}</p>;
  } else if (shown.length === 0) {
    body = (
      <p className="text-callout text-fg-secondary">{t("ext.none", { query: query.trim() })}</p>
    );
  } else {
    body = (
      <ul className="flex max-h-72 flex-col overflow-y-auto" aria-label={t("ext.title")}>
        {shown.map(({ extension }) => {
          const failure = failures[extension.name];
          const enabled = shownEnabled(extension, pending);
          const cli = (enabled ? disablePhpExtension : enablePhpExtension).cli({
            version: series,
            extension: extension.name,
          });
          return (
            <li
              key={extension.name}
              data-extension={extension.name}
              data-enabled={enabled}
              data-pending={pending[extension.name] === undefined ? undefined : "true"}
              data-forge-cli={cli ?? undefined}
              className="flex flex-col gap-1 py-1"
            >
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate font-mono text-mono">
                  {extension.name}
                </span>
                <Badge tone={extension.bundled ? "neutral" : "info"}>
                  {extension.bundled ? t("ext.bundled") : t("ext.registry")}
                </Badge>
                {extension.zend ? (
                  <Tooltip content={t("ext.zendHint")}>
                    <span data-testid="zend-marker">
                      <Badge tone="accent">{t("ext.zend")}</Badge>
                    </span>
                  </Tooltip>
                ) : null}
                <Toggle
                  label={t("ext.toggle", { name: extension.name })}
                  checked={enabled}
                  disabled={!capability.available}
                  onCheckedChange={(next) => {
                    toggle(extension.name, next);
                  }}
                />
              </div>
              {failure === undefined ? null : (
                <div role="alert" className="flex flex-col gap-1">
                  <p className="text-caption text-state-failed">
                    {t("ext.failed", { name: extension.name })}
                  </p>
                  <ErrorNotice error={failure} />
                </div>
              )}
            </li>
          );
        })}
      </ul>
    );
  }

  return (
    <section aria-label={t("ext.title")} data-testid="extensions" className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="text-headline">{t("ext.title")}</h3>
        {list.data === undefined ? null : (
          <span className={`text-caption text-fg-secondary ${TNUM_CLASS}`}>
            {t("ext.count", { shown: shown.length, total: index.length })}
          </span>
        )}
      </div>
      <input
        type="search"
        aria-label={t("ext.search")}
        placeholder={t("ext.searchPlaceholder")}
        value={query}
        autoComplete="off"
        spellCheck={false}
        onChange={(event) => {
          setQuery(event.target.value);
        }}
        className="h-8 w-full rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      />
      {body}
    </section>
  );
}
