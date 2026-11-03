import { useTranslation } from "react-i18next";
import { pickPath, type PickKind } from "../../ipc/native";
import { useHello } from "../../state";
import { Button } from "./Button";
import { TextField, type TextFieldProps } from "./TextField";

export interface PathFieldProps extends Omit<TextFieldProps, "value" | "onChange" | "hint"> {
  value: string;
  onChange: (value: string) => void;
  /** What "Procurar…" picks (default a folder). */
  kind?: PickKind;
}

/**
 * Path input. "Procurar…" opens the native dialog only when the daemon is local
 * (`hello.local`): a path picked on this machine means nothing to a remote daemon, so there
 * the field is free text with an explanation (docs/07 section 6, ADR-0009).
 */
export function PathField({ value, onChange, kind = "directory", ...rest }: PathFieldProps) {
  const { t } = useTranslation("common");
  const hello = useHello();
  const local = hello?.local === true;
  const remote = hello !== null && !hello.local;
  const browse = async () => {
    const picked = await pickPath(kind, value === "" ? undefined : value);
    if (picked !== null) onChange(picked);
  };
  return (
    <div className="flex items-end gap-2" data-path-field={local ? "local" : "text"}>
      <TextField
        {...rest}
        value={value}
        onChange={(event) => {
          onChange(event.target.value);
        }}
        {...(remote ? { hint: t("ui.pathRemoteHint") } : {})}
      />
      {local ? (
        <Button
          onClick={() => {
            void browse();
          }}
        >
          {t("ui.pathBrowse")}
        </Button>
      ) : null}
    </div>
  );
}
