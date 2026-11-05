import { useState } from "react";
import { useTranslation } from "react-i18next";
import { importDatabase } from "../../actions/databases";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { PathField } from "../../design/primitives/PathField";
import { Sheet } from "../../design/primitives/Sheet";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import { useCapability, useHello, useRuntime } from "../../state";
import { importProblem } from "./model";
import { startTransfer } from "./transfers";

export interface ImportTarget {
  instance: string;
  name: string;
}

/**
 * Import into one database: a local daemon gets the native file picker; a remote one reads only
 * inside its own `www/`, so the field is free text with that explained (docs/11 §5, ADR-0009).
 * The job's percentage and cancel button show in the transfer list once it starts.
 */
export function ImportSheet({
  target,
  onClose,
}: {
  target: ImportTarget | null;
  onClose: () => void;
}) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const hello = useHello();
  const capability = useCapability(importDatabase.method);
  const remote = hello !== null && !hello.local;
  const [file, setFile] = useState("");
  const [touched, setTouched] = useState(false);
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const problem = importProblem(file, remote);

  const close = () => {
    setFile("");
    setTouched(false);
    setFailure(null);
    onClose();
  };
  const submit = () => {
    setTouched(true);
    if (target === null || problem !== undefined || starting) return;
    setStarting(true);
    setFailure(null);
    startTransfer(runtime, {
      kind: "import",
      instance: target.instance,
      name: target.name,
      file,
    }).then(
      () => {
        setStarting(false);
        close();
      },
      (error: unknown) => {
        setStarting(false);
        setFailure(toForgeApiError(error, { method: "db.import" }));
      },
    );
  };
  const command =
    target === null || problem !== undefined ? null : importDatabase.cli({ ...target, file });
  return (
    <Sheet
      open={target !== null}
      onOpenChange={(open) => {
        if (!open) close();
      }}
      title={t("import.title", { name: target?.name ?? "" })}
      description={t("import.description")}
    >
      <div className="flex flex-col gap-3 p-4" data-testid="import-sheet">
        {remote ? (
          <p
            role="note"
            data-testid="import-remote-note"
            className="text-callout text-fg-secondary"
          >
            {t("import.remoteNote")}
          </p>
        ) : null}
        <PathField
          label={t("import.file")}
          kind="file"
          width="full"
          value={file}
          onChange={(value) => {
            setFile(value);
            setTouched(true);
          }}
          spellCheck={false}
          {...(touched && problem !== undefined ? { error: t(`import.${problem}`) } : {})}
        />
        <CliCommand command={command} />
        {failure === null ? null : <ErrorNotice error={failure} />}
        <div className="flex justify-end">
          <Button
            variant="primary"
            loading={starting}
            loadingLabel={t("import.starting")}
            disabled={!capability.available}
            onClick={submit}
          >
            {t("import.submit")}
          </Button>
        </div>
      </div>
    </Sheet>
  );
}
