import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { enableDns } from "../../../../actions/diagnostics";
import { runAction } from "../../../../actions/registry";
import { setConfig } from "../../../../actions/system";
import { Button } from "../../../../design/primitives/Button";
import { CliCommand } from "../../../../design/primitives/CliCommand";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { TextField } from "../../../../design/primitives/TextField";
import { newIntent } from "../../../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../../../ipc/errors";
import type { ConfigValueDto } from "../../../../ipc/generated";
import { useCapability, useRuntime } from "../../../../state";
import { RowFrame } from "../../RowFrame";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { JobButton } from "../common/JobButton";
import { JobStatus } from "../common/JobStatus";
import { RotateCa } from "../common/RotateCa";
import { useJobAction } from "../common/useJobAction";
import { normalizeTld } from "./model";

const CAPTION_FORGE_CONFIG_SET_TLD = "forge config set tld";
const KEY_TLD = "tld";
const METHOD_NET_DNS_ENABLE = "net.dns_enable";
const METHOD_SYSTEM_CONFIG_SET = "system.config_set";
const TID_TLD_DIALOG = "tld-dialog";
const TID_TLD_DNS_STATUS = "tld-dns-status";
const TID_TLD_ERROR = "tld-error";
const TID_TLD_REQUIRES = "tld-requires";
const TID_TLD_ROTATE = "tld-rotate";

export interface TldRowProps {
  /** `tld` of `system.config_list`. */
  value: string;
}

/** Follow-up the daemon asked for after the TLD changed (`ConfigValueDto.requires`). */
function FollowUps({ methods }: { methods: readonly string[] }) {
  const { t } = useTranslation("settings");
  const dns = useJobAction(enableDns);
  if (methods.length === 0) return null;
  return (
    <ul data-testid={TID_TLD_REQUIRES} className="flex w-full flex-col gap-2">
      {methods.includes("tls.ca_rotate") ? (
        <li className="flex flex-col gap-2">
          <span className="text-callout text-fg-secondary">{t("network.tld.needsRotate")}</span>
          <RotateCa testId={TID_TLD_ROTATE} />
        </li>
      ) : null}
      {methods.includes("net.dns_enable") ? (
        <li className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2 text-callout">
            <span className="text-fg-secondary">{t("network.tld.needsDns")}</span>
            <JobButton action={enableDns} input={undefined} job={dns}>
              {t("network.dns.enable")}
            </JobButton>
          </div>
          <JobStatus
            job={dns}
            label={t("network.dns.enabling")}
            method={METHOD_NET_DNS_ENABLE}
            testId={TID_TLD_DNS_STATUS}
            elevated
          />
        </li>
      ) : null}
    </ul>
  );
}

/**
 * TLD of local sites. A change reissues every certificate (the CA name constraints and the
 * SANs name the TLD), so it asks first and says so; "Cancel" puts the old value back and calls
 * nothing. After the save, the follow-ups the daemon names (`tls.ca_rotate`, `net.dns_enable`)
 * appear as buttons (rotation behind its own destructive modal).
 */
export function TldRow({ value }: TldRowProps) {
  const { t } = useTranslation("settings");
  const runtime = useRuntime();
  const capability = useCapability("system.config_set");
  const [draft, setDraft] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<ForgeApiError | null>(null);
  const [requires, setRequires] = useState<readonly string[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const shown = draft ?? value;
  const dirty = draft !== null && draft.trim().toLowerCase() !== value;

  const ask = () => {
    const tld = normalizeTld(shown);
    if (tld === null) {
      setProblem(t("network.tld.invalid"));
      return;
    }
    setProblem(null);
    if (tld === value) {
      setDraft(null);
      return;
    }
    setPending(tld);
  };
  const cancel = () => {
    setPending(null);
    setDraft(null);
    setProblem(null);
  };
  const confirm = () => {
    const tld = pending;
    if (tld === null) return;
    setPending(null);
    setSaving(true);
    setError(null);
    setRequires([]);
    runAction(runtime.queue, setConfig, { key: KEY_TLD, value: tld }, { intent: newIntent() }).then(
      (result) => {
        if (!alive.current) return;
        setSaving(false);
        setDraft(null);
        setRequires((result as ConfigValueDto).requires ?? []);
      },
      (failure: unknown) => {
        if (!alive.current) return;
        setSaving(false);
        setDraft(null);
        setError(toForgeApiError(failure, { method: METHOD_SYSTEM_CONFIG_SET }));
      },
    );
  };

  return (
    <RowFrame
      id="tld"
      label={t("network.tld.label")}
      description={t("network.tld.description")}
      caption={CAPTION_FORGE_CONFIG_SET_TLD}
    >
      <div className="flex flex-col items-end gap-2">
        <div className="flex flex-wrap items-end gap-2">
          <TextField
            label={t("network.tld.label")}
            width="sm"
            value={shown}
            disabled={capability.text !== null || saving}
            {...(problem === null ? {} : { error: problem })}
            onChange={(event) => {
              setDraft(event.target.value);
              setProblem(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && dirty) ask();
            }}
          />
          <Button
            variant="primary"
            loading={saving}
            loadingLabel={t("saving")}
            disabled={!dirty || capability.text !== null}
            onClick={ask}
          >
            {t("save")}
          </Button>
        </div>
        {error === null ? null : (
          <div data-testid={TID_TLD_ERROR}>
            <ErrorNotice error={error} context={{ method: METHOD_SYSTEM_CONFIG_SET }} />
          </div>
        )}
        {dirty ? (
          <CliCommand
            command={setConfig.cli({ key: KEY_TLD, value: normalizeTld(shown) ?? shown })}
          />
        ) : null}
        <FollowUps methods={requires} />
      </div>
      <ConfirmDialog
        open={pending !== null}
        onOpenChange={(open) => {
          if (!open) cancel();
        }}
        title={t("network.tld.confirmTitle", { tld: pending ?? "" })}
        description={t("network.tld.confirmBody", { from: value, to: pending ?? "" })}
        confirmLabel={t("network.tld.confirm")}
        command={pending === null ? null : setConfig.cli({ key: KEY_TLD, value: pending })}
        testId={TID_TLD_DIALOG}
        onConfirm={confirm}
      />
    </RowFrame>
  );
}
