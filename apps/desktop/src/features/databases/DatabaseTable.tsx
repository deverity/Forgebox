import { useState } from "react";
import { useTranslation } from "react-i18next";
import { exportDatabase } from "../../actions/databases";
import { useCapabilityReason } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { Tooltip } from "../../design/primitives/Tooltip";
import { useFormatters } from "../../design/primitives/useFormatters";
import { TNUM_CLASS } from "../../i18n/format";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type * as T from "../../ipc/generated";
import { useCapability, useRuntime } from "../../state";
import { ConnectionPopover } from "./ConnectionPopover";
import { DropDialog, type DropTarget } from "./DropDialog";
import { ImportSheet, type ImportTarget } from "./ImportSheet";
import { charsetText } from "./model";
import { ShellButton } from "./ShellButton";
import { startTransfer } from "./transfers";

function ExportButton({
  instance,
  name,
  onError,
}: {
  instance: string;
  name: string;
  onError: (error: ForgeApiError) => void;
}) {
  const { t } = useTranslation("databases");
  const runtime = useRuntime();
  const capability = useCapability(exportDatabase.method);
  const reason = useCapabilityReason(capability);
  const [starting, setStarting] = useState(false);
  const input = { instance, name };
  const command = exportDatabase.cli(input) ?? "";
  return (
    <Tooltip content={reason ?? t("export.action")} command={command}>
      <span data-forge-cli={command} data-action="db.export" className="inline-flex">
        <Button
          variant="ghost"
          loading={starting}
          disabled={reason !== null}
          onClick={() => {
            setStarting(true);
            startTransfer(runtime, { kind: "export", instance, name }).then(
              () => {
                setStarting(false);
              },
              (error: unknown) => {
                setStarting(false);
                onError(toForgeApiError(error, { method: exportDatabase.method }));
              },
            );
          }}
        >
          {t("export.action")}
        </Button>
      </span>
    </Tooltip>
  );
}

/**
 * The databases of one running instance: name, charset / collation, size and tables, with the
 * actions of each row. "Abrir em" (`db.open`) is M3-E1 and shows disabled as "em breve".
 */
export function DatabaseTable({
  instance,
  databases,
  onError,
}: {
  instance: T.DbInstanceDto;
  databases: readonly T.DatabaseDto[];
  onError: (error: ForgeApiError) => void;
}) {
  const { t } = useTranslation("databases");
  const format = useFormatters();
  const caps = instance.capabilities ?? {};
  const [drop, setDrop] = useState<DropTarget | null>(null);
  const [importing, setImporting] = useState<ImportTarget | null>(null);
  const unknown = t("table.unknown");
  return (
    <section
      aria-label={t("table.title", { name: instance.instance })}
      data-testid="database-table"
      className="flex flex-col gap-2"
    >
      <div className="flex items-center gap-2">
        <h2 className="flex-1 text-headline">{t("table.title", { name: instance.instance })}</h2>
        <span title={t("openWith.reason")} className="inline-flex">
          <Button variant="secondary" disabled>
            {t("openWith.disabled")}
          </Button>
        </span>
      </div>
      {databases.length === 0 ? (
        <p className="py-4 text-callout text-fg-secondary">{t("table.none")}</p>
      ) : (
        <div className="overflow-x-auto rounded-card border border-stroke">
          <table className="w-full text-left text-body">
            <thead className="text-callout text-fg-secondary">
              <tr className="border-b border-stroke">
                <th scope="col" className="px-3 py-2 font-medium">
                  {t("table.name")}
                </th>
                <th scope="col" className="px-3 py-2 font-medium">
                  {t("table.charset")}
                </th>
                <th scope="col" className="px-3 py-2 text-right font-medium">
                  {t("table.size")}
                </th>
                <th scope="col" className="px-3 py-2 text-right font-medium">
                  {t("table.tables")}
                </th>
                <th scope="col" className="px-3 py-2 font-medium">
                  <span className="sr-only">{t("table.actions")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {databases.map((db) => (
                <tr
                  key={db.name}
                  data-database={db.name}
                  className="border-b border-stroke last:border-b-0"
                >
                  <th scope="row" className="px-3 py-2 font-mono text-mono font-normal">
                    {db.name}
                  </th>
                  <td className="px-3 py-2 text-callout text-fg-secondary">
                    {charsetText(db) ?? unknown}
                  </td>
                  <td className={`px-3 py-2 text-right ${TNUM_CLASS}`}>
                    {db.size_bytes === null ? unknown : format.bytes(db.size_bytes)}
                  </td>
                  <td className={`px-3 py-2 text-right ${TNUM_CLASS}`}>
                    {db.tables === null ? unknown : format.number(db.tables)}
                  </td>
                  <td className="px-3 py-2">
                    <div className="flex flex-wrap justify-end gap-1">
                      {caps.url === false ? null : (
                        <ConnectionPopover
                          target={{
                            instance: instance.instance,
                            protocol: instance.protocol,
                            database: db.name,
                          }}
                          trigger={<Button variant="ghost">{t("connect.action")}</Button>}
                        />
                      )}
                      {caps.shell === false ? null : (
                        <ShellButton
                          instance={instance.instance}
                          database={db.name}
                          title={t("shell.title", { name: db.name })}
                        />
                      )}
                      {caps.export === false ? null : (
                        <ExportButton
                          instance={instance.instance}
                          name={db.name}
                          onError={onError}
                        />
                      )}
                      {caps.import === false ? null : (
                        <Button
                          variant="ghost"
                          onClick={() => {
                            setImporting({ instance: instance.instance, name: db.name });
                          }}
                        >
                          {t("import.action")}
                        </Button>
                      )}
                      {caps.drop === false ? null : (
                        <Button
                          variant="destructive"
                          aria-label={`${t("drop.action")} ${db.name}`}
                          onClick={() => {
                            setDrop({ instance: instance.instance, name: db.name });
                          }}
                        >
                          {t("drop.action")}
                        </Button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <DropDialog
        target={drop}
        onClose={() => {
          setDrop(null);
        }}
      />
      <ImportSheet
        target={importing}
        onClose={() => {
          setImporting(null);
        }}
      />
    </section>
  );
}
