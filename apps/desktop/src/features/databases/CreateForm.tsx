import { useState } from "react";
import { useTranslation } from "react-i18next";
import { createDatabase } from "../../actions/databases";
import { ActionButton } from "../../design/primitives/ActionButton";
import { CliCommand } from "../../design/primitives/CliCommand";
import { Select } from "../../design/primitives/Select";
import { TextField } from "../../design/primitives/TextField";
import { charsetOptions, nameProblem, type NameProblem } from "./model";

export interface CreateFormProps {
  /** `mysql/default`. */
  instance: string;
  protocol: string;
  /** Names that already exist on the instance (compared exactly). */
  existing: readonly string[];
  /** Starts with this name (the site tab suggests one). */
  initialName?: string;
  onCreated?: (name: string) => void;
}

/**
 * Create form: the name is checked against the daemon's regex while typing and again before the
 * call, so an invalid name never leaves the screen. Names are case-sensitive (docs/15).
 */
export function CreateForm({
  instance,
  protocol,
  existing,
  initialName = "",
  onCreated,
}: CreateFormProps) {
  const { t } = useTranslation("databases");
  const charsets = charsetOptions(protocol);
  const [name, setName] = useState(initialName);
  const [charset, setCharset] = useState<string>(charsets[0] ?? "");
  const [touched, setTouched] = useState(initialName !== "");
  const problem: NameProblem | undefined = nameProblem(name, existing);
  const effectiveCharset = charsets.includes(charset) ? charset : (charsets[0] ?? "");
  const input = { instance, name, charset: effectiveCharset === "" ? null : effectiveCharset };
  return (
    <form
      aria-label={t("create.title")}
      data-testid="create-database"
      className="flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      <div className="flex flex-wrap items-start gap-3">
        <TextField
          label={t("create.name")}
          hint={t("create.hint")}
          value={name}
          autoComplete="off"
          spellCheck={false}
          maxLength={80}
          onChange={(event) => {
            setName(event.target.value);
            setTouched(true);
          }}
          {...(touched && problem !== undefined ? { error: t(`create.${problem}`) } : {})}
        />
        <Select
          label={t("create.charset")}
          options={charsets.map((value) => ({ value, label: value }))}
          value={effectiveCharset}
          onValueChange={setCharset}
          width="sm"
        />
        <div className="pt-6">
          <ActionButton
            action={createDatabase}
            input={input}
            variant="primary"
            loadingLabel={t("create.creating")}
            successLabel={t("create.done")}
            {...(problem === undefined ? {} : { disabledReason: t(`create.${problem}`) })}
            onDone={() => {
              onCreated?.(name);
              setName("");
              setTouched(false);
            }}
          >
            {t("create.submit")}
          </ActionButton>
        </div>
      </div>
      <CliCommand command={problem === undefined ? createDatabase.cli(input) : null} />
    </form>
  );
}
