import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../../../design/primitives/Badge";
import { Button } from "../../../design/primitives/Button";
import { EmptyState } from "../../../design/primitives/EmptyState";
import { ErrorNotice } from "../../../design/primitives/ErrorNotice";
import { Skeleton } from "../../../design/primitives/Skeleton";
import { copyText } from "../../../ipc/native";
import { toForgeApiError, type ForgeApiError } from "../../../ipc/errors";
import type * as T from "../../../ipc/generated";
import { useForgeApi, useForgeQuery, useSite } from "../../../state";
import { checkTone } from "./model";

function CopyFix({ fix }: { fix: string }) {
  const { t } = useTranslation("sites");
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <div className="flex items-center gap-2 rounded-control border border-stroke bg-surface px-2 py-1">
      <code
        data-testid="check-fix"
        className="min-w-0 flex-1 truncate font-mono text-mono text-fg-secondary"
      >
        {fix}
      </code>
      <Button
        variant="ghost"
        onClick={() => {
          void copyText(fix).then((ok) => {
            setState(ok ? "copied" : "failed");
          });
        }}
      >
        {state === "copied"
          ? t("detail.diagnostics.copied")
          : state === "failed"
            ? t("detail.diagnostics.copyFailed")
            : t("detail.diagnostics.copyFix")}
      </Button>
    </div>
  );
}

function CheckRow({ check }: { check: T.CheckDto }) {
  const { t } = useTranslation("sites");
  return (
    <li
      data-check={check.id}
      data-status={check.status}
      className="flex flex-col gap-2 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex items-center gap-2">
        <Badge tone={checkTone(check.status)}>
          {t(`detail.diagnostics.status.${check.status}`)}
        </Badge>
        <span className="font-mono text-mono text-fg-secondary">{check.id}</span>
      </div>
      <p className="text-body text-fg-primary">{check.message}</p>
      {check.fix !== null && check.status !== "ok" ? (
        <div className="flex flex-col gap-1">
          <span className="text-caption text-fg-tertiary">{t("detail.diagnostics.fix")}</span>
          <CopyFix fix={check.fix} />
        </div>
      ) : null}
    </li>
  );
}

type DnsState =
  | { status: "idle" }
  | { status: "running" }
  | { status: "done"; result: T.DnsTestDto }
  | { status: "failed"; error: ForgeApiError };

/** "Testar DNS": `net.dns_test` for the host, shown inline (a read, not a mutation). */
function DnsTest({ host }: { host: string }) {
  const { t } = useTranslation("sites");
  const api = useForgeApi();
  const [state, setState] = useState<DnsState>({ status: "idle" });
  const run = () => {
    setState({ status: "running" });
    api.call("net.dns_test", { host }).then(
      (result) => {
        setState({ status: "done", result });
      },
      (error: unknown) => {
        setState({ status: "failed", error: toForgeApiError(error, { method: "net.dns_test" }) });
      },
    );
  };
  return (
    <div className="flex flex-col gap-2" data-testid="dns-test">
      <div>
        <Button variant="secondary" disabled={state.status === "running"} onClick={run}>
          {state.status === "running"
            ? t("detail.diagnostics.dns.testing")
            : t("detail.diagnostics.dns.test")}
        </Button>
      </div>
      {state.status === "done" ? (
        <p role="status" data-dns-ok={state.result.ok} className="text-callout text-fg-primary">
          {t(state.result.ok ? "detail.diagnostics.dns.ok" : "detail.diagnostics.dns.failed", {
            host,
            via: state.result.via,
            addresses:
              state.result.resolved.length === 0
                ? t("detail.diagnostics.dns.none")
                : state.result.resolved.join(", "),
          })}
        </p>
      ) : null}
      {state.status === "failed" ? <ErrorNotice error={state.error} onRetry={run} /> : null}
    </div>
  );
}

/** "Diagnóstico": the checks of `sites.info` (ok/warn/fail) with copyable fixes. */
export function DiagnosticsTab({ siteId }: { siteId: string }) {
  const { t } = useTranslation("sites");
  const site = useSite(siteId);
  const query = useForgeQuery("sites.info", { site: siteId });
  if (query.isError) {
    return (
      <ErrorNotice
        error={query.error}
        onRetry={() => {
          void query.refetch();
        }}
      />
    );
  }
  if (query.data === undefined) {
    return (
      <div aria-busy="true" data-testid="diagnostics-loading">
        <span className="sr-only">{t("detail.diagnostics.loading")}</span>
        <Skeleton height={120} />
      </div>
    );
  }
  const checks = query.data.checks;
  return (
    <div className="flex max-w-3xl flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <DnsTest host={site?.host ?? query.data.site.host} />
        <Button
          variant="ghost"
          disabled={query.isFetching}
          onClick={() => {
            void query.refetch();
          }}
        >
          {t("detail.diagnostics.run")}
        </Button>
      </div>
      {checks.length === 0 ? (
        <EmptyState title={t("detail.diagnostics.empty")} />
      ) : (
        <ul aria-label={t("detail.diagnostics.title")} className="flex flex-col gap-2">
          {checks.map((check) => (
            <CheckRow key={check.id} check={check} />
          ))}
        </ul>
      )}
    </div>
  );
}
