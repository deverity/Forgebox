import { useState } from "react";
import { useTranslation } from "react-i18next";
import { openServiceConfig } from "../../../actions/services";
import { ActionButton } from "../../../design/primitives/ActionButton";
import { Badge } from "../../../design/primitives/Badge";
import { Button } from "../../../design/primitives/Button";
import { useForgeQuery, useHello } from "../../../state";
import { QuickSettingsForm } from "../../shared/quick-settings";
import { FileViewer } from "./FileViewer";
import { QueryBoundary } from "./QueryBoundary";

export interface ConfigTabProps {
  /** `mysql/default` or `php@8.4`. */
  target: string;
}

/**
 * Configuracao: the quick-settings form, the config files of the target and "Abrir arquivo"
 * (the editor when the daemon is local, a read-only `fs.read` viewer when it is remote).
 */
export function ConfigTab({ target }: ConfigTabProps) {
  const { t } = useTranslation("services");
  const hello = useHello();
  const remote = hello !== null && !hello.local;
  const settings = useForgeQuery("services.settings", { target });
  const files = useForgeQuery("services.config_files", { target });
  const [viewing, setViewing] = useState<string | null>(null);
  return (
    <div className="flex flex-col gap-5">
      <QueryBoundary query={settings} height={200}>
        {(dto) => (
          <QuickSettingsForm
            key={JSON.stringify([dto.settings, dto.ports])}
            target={dto.target}
            settings={dto.settings}
            ports={dto.ports ?? []}
          />
        )}
      </QueryBoundary>
      <section aria-labelledby="config-files" className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-2">
          <h3 id="config-files" className="text-headline">
            {t("config.files")}
          </h3>
          {remote ? null : (
            <ActionButton
              action={openServiceConfig}
              input={target}
              loadingLabel={t("config.opening")}
              successLabel={t("config.opened")}
            >
              {t("config.open")}
            </ActionButton>
          )}
        </div>
        <QueryBoundary query={files} height={80}>
          {(dto) =>
            dto.files.length === 0 ? (
              <p className="text-callout text-fg-secondary">{t("config.noFiles")}</p>
            ) : (
              <ul className="flex flex-col gap-2">
                {dto.files.map((file) => (
                  <li
                    key={`${file.role}:${file.path}`}
                    data-config-file={file.role}
                    className="flex items-center gap-2"
                  >
                    <div className="flex min-w-0 flex-1 flex-col">
                      <span className="text-callout">{file.role}</span>
                      <span className="truncate font-mono text-mono text-fg-secondary">
                        {file.path}
                      </span>
                    </div>
                    {file.managed ? <Badge tone="accent">{t("config.managed")}</Badge> : null}
                    {remote ? (
                      <Button
                        variant="secondary"
                        disabled={file.fs_path === null}
                        aria-label={t("config.openRole", { role: file.role })}
                        onClick={() => {
                          setViewing(file.fs_path);
                        }}
                      >
                        {t("config.open")}
                      </Button>
                    ) : null}
                  </li>
                ))}
              </ul>
            )
          }
        </QueryBoundary>
        {remote && viewing !== null ? (
          <FileViewer
            key={viewing}
            fsPath={viewing}
            onClose={() => {
              setViewing(null);
            }}
          />
        ) : null}
      </section>
    </div>
  );
}
