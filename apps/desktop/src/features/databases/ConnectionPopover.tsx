import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { databaseUrl } from "../../actions/databases";
import { runAction } from "../../actions/registry";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { Popover } from "../../design/primitives/Popover";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type * as T from "../../ipc/generated";
import { copyText } from "../../ipc/native";
import { useRuntime } from "../../state";
import { shownStrings } from "./model";

export interface ConnectionTarget {
  /** `mysql/default`. */
  instance: string;
  /** Protocol family (`mysql`, `postgresql`). */
  protocol: string;
  database: string;
}

function StringBlock({
  id,
  label,
  value,
  revealed,
}: {
  id: string;
  label: string;
  value: string;
  revealed: boolean;
}) {
  const { t } = useTranslation("databases");
  const [copied, setCopied] = useState(false);
  return (
    <div data-connection={id} className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <span className="flex-1 text-callout text-fg-secondary">{label}</span>
        <Button
          variant="ghost"
          disabled={!revealed}
          title={revealed ? undefined : t("connect.copyNeedsReveal")}
          onClick={() => {
            void copyText(value).then(setCopied);
          }}
        >
          {copied ? t("connect.copied") : t("connect.copy")}
        </Button>
      </div>
      <pre
        data-testid={`connection-${id}`}
        className="overflow-x-auto rounded-control border border-stroke bg-surface-2 p-2 font-mono text-mono text-fg-primary"
      >
        {value}
      </pre>
    </div>
  );
}

/**
 * The strings are asked for when the popover opens and kept only in this component: nothing
 * with credentials goes to the query cache or a store. The credentials stay hidden until the
 * user reveals them; closing the popover forgets both the strings and the reveal.
 */
export function ConnectionStrings({ target }: { target: ConnectionTarget }) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const [result, setResult] = useState<T.DbUrlDto | null>(null);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const input = { instance: target.instance, database: target.database };

  useEffect(() => {
    let alive = true;
    runAction(
      runtime.queue,
      databaseUrl,
      { instance: target.instance, database: target.database },
      { intent: newIntent() },
    ).then(
      (answer) => {
        if (alive) setResult(answer as T.DbUrlDto);
      },
      (error: unknown) => {
        if (alive) setFailure(toForgeApiError(error, { method: "db.url" }));
      },
    );
    return () => {
      alive = false;
    };
  }, [runtime, target.instance, target.database, attempt]);

  if (failure !== null) {
    return (
      <div className="flex flex-col gap-2">
        <ErrorNotice
          error={failure}
          onRetry={() => {
            setFailure(null);
            setAttempt((n) => n + 1);
          }}
        />
      </div>
    );
  }
  if (result === null) {
    return (
      <p role="status" className="text-callout text-fg-secondary">
        {t("connect.loading")}
      </p>
    );
  }
  const shown = shownStrings(target.protocol, result, revealed);
  return (
    <div
      data-revealed={revealed}
      className="flex max-h-[min(60vh,var(--radix-popover-content-available-height))] w-[420px] max-w-full flex-col gap-3 overflow-y-auto"
    >
      <div className="flex items-center gap-2">
        <span className="flex-1 text-caption text-fg-tertiary">
          {revealed ? null : t("connect.masked")}
        </span>
        <Button
          variant="secondary"
          aria-pressed={revealed}
          onClick={() => {
            setRevealed((v) => !v);
          }}
        >
          {revealed ? t("connect.hide") : t("connect.reveal")}
        </Button>
      </div>
      {shown.dsn === null ? null : (
        <StringBlock id="dsn" label={t("connect.dsn")} value={shown.dsn} revealed />
      )}
      <StringBlock id="url" label={t("connect.url")} value={shown.url} revealed={revealed} />
      <StringBlock id="env" label={t("connect.env")} value={shown.env} revealed={revealed} />
      <CliCommand command={databaseUrl.cli(input)} />
    </div>
  );
}

/** "Conexão": DSN, URL and `.env` of one database in a popover (docs/07 §5.6). */
export function ConnectionPopover({
  target,
  trigger,
}: {
  target: ConnectionTarget;
  trigger?: React.ReactElement;
}) {
  const { t } = useTranslation("databases");
  const [open, setOpen] = useState(false);
  return (
    <Popover
      label={t("connect.label", { name: target.database })}
      open={open}
      onOpenChange={setOpen}
      align="end"
      trigger={trigger ?? <Button variant="secondary">{t("connect.action")}</Button>}
    >
      {open ? <ConnectionStrings target={target} /> : null}
    </Popover>
  );
}
