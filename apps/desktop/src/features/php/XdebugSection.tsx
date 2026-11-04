import { useState } from "react";
import { useTranslation } from "react-i18next";
import { setPhpXdebug } from "../../actions/php";
import { startJobAction } from "../../actions/registry";
import { ActionButton } from "../../design/primitives/ActionButton";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { TextField } from "../../design/primitives/TextField";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type { PhpVersionDto, XdebugMode } from "../../ipc/generated";
import { useCapability, useForgeQuery, useRuntime } from "../../state";
import { InstallProgressList } from "../shared/install-progress";
import { IDE_KEY_DEFAULT, XDEBUG_DEFAULT_PORT, XDEBUG_MODES, parseXdebugPort } from "./model";

function isMode(value: string): value is XdebugMode {
  return (XDEBUG_MODES as readonly string[]).includes(value);
}

/**
 * Xdebug of one series: off / on / trigger, the client port and the IDE key. Choosing a mode
 * on a series without the extension runs `php.xdebug`, which installs it first; that job's
 * progress shows here until the mode is set (docs/03 section 2.3).
 */
export function XdebugSection({ version }: { version: PhpVersionDto }) {
  const { t } = useTranslation("php");
  const runtime = useRuntime();
  const capability = useCapability(setPhpXdebug.method);
  const list = useForgeQuery("php.list", {});
  const series = version.series;
  const [pending, setPending] = useState<XdebugMode | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const [portText, setPortText] = useState("");

  const mode = pending ?? version.xdebug;
  const port = parseXdebugPort(portText);

  const apply = async (next: XdebugMode) => {
    setPending(next);
    setFailure(null);
    try {
      const handle = await startJobAction(
        runtime.queue,
        setPhpXdebug,
        { version: series, mode: next, port: port ?? null },
        { intent: newIntent() },
      );
      setJobId(handle.job.id);
      await handle.done;
      // The mode shown stays optimistic until `php.list` carries the new one.
      await list.refetch();
    } catch (error) {
      setFailure(toForgeApiError(error, { method: setPhpXdebug.method }));
    } finally {
      setPending(null);
      setJobId(null);
    }
  };

  const options = XDEBUG_MODES.map((value) => ({ value, label: t(`xdebug.${value}`) }));
  const portReason =
    port === undefined ? t("xdebug.portInvalid") : port === null ? t("xdebug.portHint") : undefined;
  const blocked = mode === "off" ? t("xdebug.hint.off") : portReason;
  return (
    <section
      aria-label={t("xdebug.title")}
      data-testid="xdebug"
      data-mode={mode}
      className="flex flex-col gap-3"
    >
      <h3 className="text-headline">{t("xdebug.title")}</h3>
      <SegmentedControl
        label={t("xdebug.mode")}
        options={options}
        value={mode}
        disabled={pending !== null || !capability.available}
        onValueChange={(next) => {
          if (isMode(next) && next !== mode) void apply(next);
        }}
      />
      <p className="text-caption text-fg-secondary">{t(`xdebug.hint.${mode}`)}</p>
      {jobId === null || pending === "off" ? null : (
        <InstallProgressList
          items={[
            {
              key: jobId,
              jobId,
              label: t("xdebug.installing", { series }),
            },
          ]}
        />
      )}
      {failure === null ? null : <ErrorNotice error={failure} />}
      <div className="flex items-end gap-2">
        <TextField
          label={t("xdebug.port")}
          hint={t("xdebug.portHint")}
          inputMode="numeric"
          width="sm"
          placeholder={String(XDEBUG_DEFAULT_PORT)}
          value={portText}
          validate={(text) =>
            parseXdebugPort(text) === undefined ? t("xdebug.portInvalid") : undefined
          }
          onChange={(event) => {
            setPortText(event.target.value);
          }}
        />
        <ActionButton
          action={setPhpXdebug}
          input={{ version: series, mode, port: port ?? null }}
          loadingLabel={t("xdebug.applying")}
          successLabel={t("xdebug.applied")}
          {...(blocked === undefined ? {} : { disabledReason: blocked })}
          onDone={() => {
            void list.refetch();
          }}
        >
          {t("xdebug.apply")}
        </ActionButton>
      </div>
      <TextField
        label={t("xdebug.ideKey")}
        hint={t("xdebug.ideKeyHint")}
        width="sm"
        readOnly
        value={IDE_KEY_DEFAULT}
      />
      <CliCommand
        command={setPhpXdebug.cli({
          version: series,
          mode,
          ...(typeof port === "number" ? { port } : {}),
        })}
      />
    </section>
  );
}
