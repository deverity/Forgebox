import { useState } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { parkDirectory } from "../../actions/onboarding";
import { setConfig } from "../../actions/system";
import { Button } from "../../design/primitives/Button";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { PathField } from "../../design/primitives/PathField";
import { TextField } from "../../design/primitives/TextField";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import { useForgeApi, useRuntime } from "../../state";
import { DEFAULT_TLD, normalizeTld, tldProblem } from "./model";
import { onboardingNative } from "./native";
import type { Wizard } from "./useWizard";

/**
 * Step 1 (local daemons only): the root, the parked directory and the TLD. "Continuar" does
 * the work in order, once each (`choices.applied` remembers it): `choose_root` (writes
 * root.toml and starts the daemon, which creates the layout), `system.config_set tld` and
 * `sites.park`. A refusal stays on this step with its cause.
 */
export function RootStep({ wizard, onDone }: { wizard: Wizard; onDone: () => void }) {
  const { t } = useTranslation("onboarding");
  const api = useForgeApi();
  const runtime = useRuntime();
  const { choices, update } = wizard;
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);

  const tldIssue = tldProblem(choices.tld);
  const rootEmpty = choices.root.trim() === "";
  const parkEmpty = choices.parkedDir.trim() === "";
  const invalid = tldIssue !== null || rootEmpty || parkEmpty;

  const apply = async () => {
    setBusy(true);
    setFailure(null);
    const applied = { ...choices.applied };
    const root = choices.root.trim();
    const dir = choices.parkedDir.trim();
    const tld = normalizeTld(choices.tld);
    try {
      if (applied.root !== root) {
        await onboardingNative().chooseRoot(root, api);
        applied.root = root;
        update({ applied: { ...applied } });
      }
      if (applied.tld !== tld && !(applied.tld === "" && tld === DEFAULT_TLD)) {
        await runAction(
          runtime.queue,
          setConfig,
          { key: "tld", value: tld },
          { intent: newIntent() },
        );
      }
      applied.tld = tld;
      update({ applied: { ...applied } });
      const park = `${dir}|${tld}`;
      if (applied.park !== park) {
        await runAction(runtime.queue, parkDirectory, { dir, tld }, { intent: newIntent() });
        applied.park = park;
        update({ applied: { ...applied } });
      }
      onDone();
    } catch (error) {
      setFailure(toForgeApiError(error));
    } finally {
      setBusy(false);
    }
  };

  const tldMessage = tldIssue === null ? undefined : t(`root.tld.${tldIssue}`);
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (!invalid && !busy) void apply();
      }}
    >
      <PathField
        label={t("root.rootLabel")}
        width="full"
        value={choices.root}
        onChange={(root) => {
          update({ root });
        }}
        showDisabledBrowse
        {...(rootEmpty ? { error: t("root.pathEmpty") } : {})}
      />
      <p className="-mt-2 text-caption text-fg-secondary">{t("root.rootHint")}</p>
      <PathField
        label={t("root.parkedLabel")}
        width="full"
        value={choices.parkedDir}
        onChange={(parkedDir) => {
          update({ parkedDir, parkedEdited: true });
        }}
        showDisabledBrowse
        {...(parkEmpty ? { error: t("root.pathEmpty") } : {})}
      />
      <p className="-mt-2 text-caption text-fg-secondary">
        {t("root.parkedHint", { tld: normalizeTld(choices.tld) || DEFAULT_TLD })}
      </p>
      <TextField
        label={t("root.tldLabel")}
        hint={t("root.tldHint")}
        width="sm"
        value={choices.tld}
        spellCheck={false}
        autoCapitalize="none"
        onChange={(event) => {
          update({ tld: event.target.value });
        }}
        {...(tldMessage === undefined ? {} : { error: tldMessage })}
      />
      {failure === null ? null : <ErrorNotice error={failure} onRetry={() => void apply()} />}
      <div className="flex justify-end">
        <Button
          type="submit"
          variant="primary"
          loading={busy}
          loadingLabel={t("nav.applying")}
          disabled={invalid}
        >
          {t("nav.next")}
        </Button>
      </div>
    </form>
  );
}
