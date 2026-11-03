import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { createInstance, removeInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Button } from "../../design/primitives/Button";
import { PathField } from "../../design/primitives/PathField";
import { PortField, parsePort } from "../../design/primitives/PortField";
import { Select } from "../../design/primitives/Select";
import { StatusDot } from "../../design/primitives/StatusDot";
import { TextField } from "../../design/primitives/TextField";
import { Toggle } from "../../design/primitives/Toggle";
import { TNUM_CLASS } from "../../i18n/format";
import type { InstanceDto, ToolDto } from "../../ipc/generated";
import { useInstances } from "../../state";
import { dotState, installedSpecs, targetOf } from "./model";

/** Instance names: lowercase, digits, `-` and `_`; they become folder and CLI names. */
export const INSTANCE_NAME = /^[a-z0-9][a-z0-9_-]{0,31}$/;

export interface InstancesTabProps {
  instance: InstanceDto;
  tool: ToolDto | undefined;
  /** An instance was removed (the sheet closes when it was the open one). */
  onRemoved: (id: string) => void;
}

interface CreateFormProps {
  instance: InstanceDto;
  tool: ToolDto | undefined;
  names: ReadonlySet<string>;
}

function CreateForm({ instance, tool, names }: CreateFormProps) {
  const { t } = useTranslation("services");
  const specs = installedSpecs(tool);
  const own = `${instance.tool}@${instance.version}`;
  const options = (specs.length === 0 ? [own] : specs).map((spec) => ({
    value: spec,
    label: spec,
  }));
  const [spec, setSpec] = useState(
    options.some((o) => o.value === own) ? own : (options[0]?.value ?? own),
  );
  const [name, setName] = useState("");
  const [port, setPort] = useState("");
  const [dataDir, setDataDir] = useState("");
  const [autostart, setAutostart] = useState(false);
  const parsed = port === "" ? null : parsePort(port);
  let nameError: string | undefined;
  if (name !== "" && !INSTANCE_NAME.test(name)) nameError = t("create.nameInvalid");
  else if (names.has(name)) nameError = t("create.nameTaken");
  let reason: string | undefined;
  if (name === "") reason = t("create.nameRequired");
  else if (nameError !== undefined) reason = nameError;
  else if (port !== "" && parsed === null) reason = t("create.portInvalid");
  return (
    <form
      data-testid="create-instance"
      aria-label={t("create.title")}
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      <h3 className="text-headline">{t("create.title")}</h3>
      <Select label={t("create.version")} options={options} value={spec} onValueChange={setSpec} />
      <TextField
        label={t("create.name")}
        value={name}
        autoComplete="off"
        {...(nameError === undefined ? {} : { error: nameError })}
        onChange={(event) => {
          setName(event.target.value);
        }}
      />
      <PortField label={t("create.port")} value={port} onChange={setPort} />
      <PathField label={t("create.dataDir")} value={dataDir} onChange={setDataDir} />
      <div className="flex items-center gap-2">
        <Toggle label={t("create.autostart")} checked={autostart} onCheckedChange={setAutostart} />
        <span className="text-callout text-fg-secondary">{t("create.autostart")}</span>
      </div>
      <div>
        <ActionButton
          variant="primary"
          action={createInstance}
          input={{ tool: spec, name, port: parsed, autostart, dataDir: dataDir.trim() }}
          loadingLabel={t("create.creating")}
          successLabel={t("create.created")}
          {...(reason === undefined ? {} : { disabledReason: reason })}
          onDone={() => {
            setName("");
            setPort("");
            setDataDir("");
            setAutostart(false);
          }}
        >
          {t("create.submit")}
        </ActionButton>
      </div>
    </form>
  );
}

interface RemoveConfirmProps {
  target: string;
  name: string;
  instanceId: string;
  onClose: () => void;
  onRemoved: (id: string) => void;
}

/** Typing the instance name unlocks the button; the request carries `confirm: true`. */
function RemoveConfirm({ target, name, instanceId, onClose, onRemoved }: RemoveConfirmProps) {
  const { t } = useTranslation("services");
  const [typed, setTyped] = useState("");
  const [deleteData, setDeleteData] = useState(false);
  const matches = typed === name;
  return (
    <div
      role="group"
      aria-label={t("remove.title", { name: target })}
      data-testid="remove-confirm"
      className="flex flex-col gap-3 rounded-card border border-state-failed p-3"
    >
      <p className="text-callout text-fg-primary">{t("remove.warning", { name: target })}</p>
      <TextField
        label={t("remove.type", { name })}
        value={typed}
        autoComplete="off"
        onChange={(event) => {
          setTyped(event.target.value);
        }}
      />
      <div className="flex items-center gap-2">
        <Toggle
          label={t("remove.deleteData")}
          checked={deleteData}
          onCheckedChange={setDeleteData}
        />
        <span className="text-callout text-fg-secondary">{t("remove.deleteData")}</span>
      </div>
      <div className="flex gap-2">
        <ActionButton
          variant="destructive"
          action={removeInstance}
          input={{ instance: target, deleteData }}
          loadingLabel={t("remove.removing")}
          successLabel={t("remove.removed")}
          {...(matches ? {} : { disabledReason: t("remove.mustType", { name }) })}
          onDone={() => {
            onRemoved(instanceId);
            onClose();
          }}
        >
          {t("remove.confirm")}
        </ActionButton>
        <Button variant="ghost" onClick={onClose}>
          {t("remove.cancel")}
        </Button>
      </div>
    </div>
  );
}

/** The tool instances, a form to add one and the typed-name confirmation to remove one. */
export function InstancesTab({ instance, tool, onRemoved }: InstancesTabProps) {
  const { t } = useTranslation("services");
  const all = useInstances();
  const [removing, setRemoving] = useState<string | null>(null);
  const siblings = useMemo(
    () =>
      Object.values(all)
        .filter((other) => other.tool === instance.tool)
        .sort((a, b) => a.name.localeCompare(b.name)),
    [all, instance.tool],
  );
  const names = useMemo(() => new Set(siblings.map((s) => s.name)), [siblings]);
  return (
    <div className="flex flex-col gap-5">
      <section aria-label={t("instances.title")} className="flex flex-col gap-2">
        <h3 className="text-headline">{t("instances.title")}</h3>
        <ul className="flex flex-col gap-2">
          {siblings.map((other) => {
            const target = targetOf(other);
            return (
              <li key={other.id} data-instance={target} className="flex flex-col gap-2">
                <div className="flex items-center gap-3">
                  <span aria-hidden="true">
                    <StatusDot state={dotState(other.state)} />
                  </span>
                  <span className="min-w-0 flex-1 truncate text-body">{target}</span>
                  <span className={`text-callout text-fg-secondary ${TNUM_CLASS}`}>
                    {other.version}
                  </span>
                  <Button
                    variant="ghost"
                    aria-label={t("instances.remove", { name: target })}
                    onClick={() => {
                      setRemoving(removing === other.id ? null : other.id);
                    }}
                  >
                    {t("instances.removeShort")}
                  </Button>
                </div>
                {removing === other.id ? (
                  <RemoveConfirm
                    target={target}
                    name={other.name}
                    instanceId={other.id}
                    onClose={() => {
                      setRemoving(null);
                    }}
                    onRemoved={onRemoved}
                  />
                ) : null}
              </li>
            );
          })}
        </ul>
      </section>
      <CreateForm instance={instance} tool={tool} names={names} />
    </div>
  );
}
