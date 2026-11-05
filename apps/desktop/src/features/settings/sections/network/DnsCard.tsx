import { useState } from "react";
import { useTranslation } from "react-i18next";
import { enableDns } from "../../../../actions/diagnostics";
import { disableDns } from "../../../../actions/security";
import { Badge } from "../../../../design/primitives/Badge";
import { Button } from "../../../../design/primitives/Button";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { TextField } from "../../../../design/primitives/TextField";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { DnsMode, DnsTestDto } from "../../../../ipc/generated";
import { renderCli } from "../../../../ipc/cli";
import { useCapability, useForgeApi, useForgeQuery } from "../../../../state";
import { loose } from "../../looseT";
import { RowFrame } from "../../RowFrame";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { useJobAction } from "../common/useJobAction";

const CAPTION_FORGE_DNS_STATUS = "forge dns status";
const DASH = "—";
const METHOD_NET_DNS_DISABLE = "net.dns_disable";
const METHOD_NET_DNS_ENABLE = "net.dns_enable";
const METHOD_NET_DNS_STATUS = "net.dns_status";
const METHOD_NET_DNS_TEST = "net.dns_test";
const TID_DNS_DISABLE_STATUS = "dns-disable-status";
const TID_DNS_ENABLE_STATUS = "dns-enable-status";
const TID_DNS_LISTEN = "dns-listen";
const TID_DNS_MODE = "dns-mode";
const TID_DNS_REASON = "dns-reason";
const TID_DNS_STATUS = "dns-status";
const TID_DNS_TEST_EXPECTED = "dns-test-expected";
const TID_DNS_TEST_RESOLVED = "dns-test-resolved";
const TID_DNS_TEST_RESULT = "dns-test-result";
const TID_DNS_TLD = "dns-tld";

const DEFAULT_HOST = "meusite.test";
/** Shown while there is no host to put in the command. */
const DNS_TEST_HINT = "forge dns test <host>";

/** Resolved vs expected of `net.dns_test`, side by side, with the path that answered. */
function TestResult({ result }: { result: DnsTestDto }) {
  const { t } = useTranslation("settings");
  const mode: DnsMode | undefined = result.mode;
  return (
    <div data-testid={TID_DNS_TEST_RESULT} data-ok={result.ok} className="flex flex-col gap-1">
      <p className="text-callout">
        <Badge tone={result.ok ? "success" : "danger"}>
          {result.ok ? t("network.dns.test.ok") : t("network.dns.test.fail")}
        </Badge>{" "}
        {t("network.dns.test.via", { via: result.via })}
      </p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-callout">
        <dt className="text-fg-tertiary">{t("network.dns.test.resolved")}</dt>
        <dd data-testid={TID_DNS_TEST_RESOLVED} className="font-mono text-mono">
          {result.resolved.length === 0 ? t("network.dns.test.none") : result.resolved.join(", ")}
        </dd>
        <dt className="text-fg-tertiary">{t("network.dns.test.expected")}</dt>
        <dd data-testid={TID_DNS_TEST_EXPECTED} className="font-mono text-mono">
          {result.expected.join(", ")}
        </dd>
        {mode === undefined ? null : (
          <>
            <dt className="text-fg-tertiary">{t("network.dns.mode")}</dt>
            <dd>{loose(t)(`network.dns.modes.${mode}`)}</dd>
          </>
        )}
      </dl>
      {result.reason === undefined || result.reason === null ? null : (
        <p className="text-callout text-fg-secondary">
          {loose(t)(`network.dns.reasons.${result.reason}`)}
        </p>
      )}
    </div>
  );
}

/**
 * Local DNS (docs/06 section 5): the server, where it listens and how Windows routes the TLD
 * (NRPT rule, or the `hosts` fallback with the reason). Enable and disable are jobs that ask
 * for administrator rights (UAC), so the card says so and shows progress while the prompt
 * waits. "Testar host" asks the daemon what a host resolves to and compares with what the
 * stack expects.
 */
export function DnsCard() {
  const { t } = useTranslation("settings");
  const api = useForgeApi();
  const status = useForgeQuery("net.dns_status", {});
  const testCapability = useCapability("net.dns_test");
  const enable = useJobAction(enableDns);
  const disable = useJobAction(disableDns);
  const [host, setHost] = useState(DEFAULT_HOST);
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<DnsTestDto | null>(null);
  const [testError, setTestError] = useState<ForgeApiError | null>(null);
  const dns = status.data;
  const trimmed = host.trim().toLowerCase();

  const test = () => {
    if (trimmed === "") return;
    setTesting(true);
    setTestError(null);
    api.call("net.dns_test", { host: trimmed }).then(
      (answer) => {
        setResult(answer);
        setTesting(false);
      },
      (failure: unknown) => {
        setResult(null);
        setTestError(toForgeApiError(failure, { method: METHOD_NET_DNS_TEST }));
        setTesting(false);
      },
    );
  };

  const policies = dns?.tld_policies ?? [];
  return (
    <RowFrame
      id="dns"
      label={t("network.dns.label")}
      description={t("network.dns.description")}
      caption={CAPTION_FORGE_DNS_STATUS}
    >
      <div className="flex w-full min-w-72 flex-col gap-3">
        {status.error !== null ? (
          <ErrorNotice
            error={status.error}
            context={{ method: METHOD_NET_DNS_STATUS }}
            onRetry={() => {
              void status.refetch();
            }}
          />
        ) : null}
        {dns === undefined ? null : (
          <dl data-testid={TID_DNS_STATUS} className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
            <dt className="text-callout text-fg-tertiary">{t("network.dns.server")}</dt>
            <dd>
              <Badge tone={dns.server_running ? "success" : "neutral"}>
                {dns.server_running ? t("network.dns.running") : t("network.dns.stopped")}
              </Badge>
            </dd>
            <dt className="text-callout text-fg-tertiary">{t("network.dns.listen")}</dt>
            <dd data-testid={TID_DNS_LISTEN} className="font-mono text-mono">
              {dns.listen ?? DASH}
            </dd>
            <dt className="text-callout text-fg-tertiary">{t("network.dns.mode")}</dt>
            <dd data-testid={TID_DNS_MODE}>
              {dns.enabled
                ? loose(t)(`network.dns.modes.${dns.mode ?? "none"}`)
                : t("network.dns.off")}
            </dd>
            {dns.reason === undefined || dns.reason === null ? null : (
              <>
                <dt className="text-callout text-fg-tertiary">{t("network.dns.reason")}</dt>
                <dd data-testid={TID_DNS_REASON} className="text-callout">
                  {loose(t)(`network.dns.reasons.${dns.reason}`)}
                </dd>
              </>
            )}
            {policies.map((policy) => (
              <div key={policy.tld} className="contents" data-testid={TID_DNS_TLD}>
                <dt className="font-mono text-mono text-fg-tertiary">{policy.tld}</dt>
                <dd className="text-callout">{loose(t)(`network.dns.modes.${policy.mode}`)}</dd>
              </div>
            ))}
          </dl>
        )}
        <div className="flex flex-wrap items-center gap-2">
          {dns?.enabled === true ? (
            <JobButton action={disableDns} input={undefined} job={disable}>
              {t("network.dns.disable")}
            </JobButton>
          ) : (
            <JobButton action={enableDns} input={undefined} job={enable} variant="primary">
              {t("network.dns.enable")}
            </JobButton>
          )}
        </div>
        <p className="text-callout text-fg-secondary">{t("network.dns.uacNote")}</p>
        <JobStatus
          job={enable}
          label={t("network.dns.enabling")}
          method={METHOD_NET_DNS_ENABLE}
          testId={TID_DNS_ENABLE_STATUS}
          elevated
        />
        <JobStatus
          job={disable}
          label={t("network.dns.disabling")}
          method={METHOD_NET_DNS_DISABLE}
          testId={TID_DNS_DISABLE_STATUS}
          elevated
        />
        <div className="flex flex-wrap items-end gap-2">
          <TextField
            label={t("network.dns.host")}
            width="md"
            value={host}
            onChange={(event) => {
              setHost(event.target.value);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") test();
            }}
          />
          <Button
            loading={testing}
            loadingLabel={t("network.dns.testing")}
            disabled={trimmed === "" || testCapability.text !== null}
            onClick={test}
          >
            {t("network.dns.testHost")}
          </Button>
        </div>
        <CliCommand
          command={
            (trimmed === "" ? null : renderCli("net.dns_test", { host: trimmed })) ?? DNS_TEST_HINT
          }
        />
        {testError === null ? null : (
          <ErrorNotice error={testError} context={{ method: METHOD_NET_DNS_TEST }} onRetry={test} />
        )}
        {result === null ? null : <TestResult result={result} />}
      </div>
    </RowFrame>
  );
}
