import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import type * as T from "../../ipc/generated";
import { useCapability, useForgeApi, useForgeQuery } from "../../state";
import { useErrorMessage } from "../../app/shell/useErrorText";

const FALLBACK_TLD = ".test";
/** A name no site uses: the test asks the resolver, not the site list. */
const PROBE_LABEL = "forgebox-doctor";

type DnsState =
  | { status: "idle" }
  | { status: "running" }
  | { status: "done"; host: string; result: T.DnsTestDto }
  | { status: "failed"; message: string };

/** "Testar DNS": `net.dns_test` for a name under the first configured TLD, shown inline. */
export function DnsTest() {
  const { t } = useTranslation("diagnostics");
  const api = useForgeApi();
  const capability = useCapability("net.dns_test");
  const status = useForgeQuery("net.dns_status", {});
  const errorMessage = useErrorMessage();
  const [state, setState] = useState<DnsState>({ status: "idle" });
  const tld = status.data?.tlds[0] ?? FALLBACK_TLD;
  const host = `${PROBE_LABEL}${tld.startsWith(".") ? tld : `.${tld}`}`;
  const run = () => {
    setState({ status: "running" });
    api.call("net.dns_test", { host }).then(
      (result) => {
        setState({ status: "done", host, result });
      },
      (error: unknown) => {
        setState({ status: "failed", message: errorMessage(error) });
      },
    );
  };
  return (
    <div className="flex items-center gap-2" data-testid="dns-test">
      <Button
        variant="secondary"
        loading={state.status === "running"}
        loadingLabel={t("dns.testing")}
        disabled={!capability.available}
        onClick={run}
      >
        {t("dns.test")}
      </Button>
      {state.status === "done" ? (
        <span
          role="status"
          data-dns-ok={state.result.ok}
          className="max-w-80 truncate text-callout text-fg-primary"
        >
          {t(state.result.ok ? "dns.ok" : "dns.failed", {
            host: state.host,
            via: state.result.via,
            addresses:
              state.result.resolved.length === 0 ? t("dns.none") : state.result.resolved.join(", "),
          })}
        </span>
      ) : null}
      {state.status === "failed" ? (
        <span role="alert" className="text-callout text-state-failed">
          {state.message}
        </span>
      ) : null}
    </div>
  );
}
