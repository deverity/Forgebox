import { useTranslation } from "react-i18next";
import { Button } from "../../../design/primitives/Button";
import { useCapability, useForgeQuery } from "../../../state";
import { markManagedLines } from "../../shared/quick-settings/model";
import { QueryBoundary } from "./QueryBoundary";

export interface FileViewerProps {
  /** Root-relative path for `fs.read` (`etc/php/8.4/php.ini`). */
  fsPath: string;
  onClose: () => void;
}

/**
 * Read-only view of a config file through `fs.read` (docs/13 §2: a remote client cannot open
 * the editor on the daemon's machine). Lines inside `# forgebox:begin/end` blocks are marked:
 * Forgebox owns them and rewrites them on the next render.
 */
export function FileViewer({ fsPath, onClose }: FileViewerProps) {
  const { t } = useTranslation("services");
  const capability = useCapability("fs.read");
  const query = useForgeQuery("fs.read", { path: fsPath });
  return (
    <section
      aria-label={t("config.viewerTitle", { path: fsPath })}
      data-testid="file-viewer"
      className="flex flex-col gap-2 rounded-card border border-stroke p-3"
    >
      <div className="flex items-center justify-between gap-2">
        <h4 className="min-w-0 truncate font-mono text-mono">{fsPath}</h4>
        <Button variant="ghost" onClick={onClose}>
          {t("config.closeViewer")}
        </Button>
      </div>
      {!capability.available ? (
        <p className="text-callout text-fg-secondary">{t("config.viewerUnavailable")}</p>
      ) : (
        <QueryBoundary query={query} height={160}>
          {(file) =>
            file.encoding !== "utf8" ? (
              <p className="text-callout text-fg-secondary">{t("config.binary")}</p>
            ) : (
              <>
                <p className="text-caption text-fg-tertiary">{t("config.managedLegend")}</p>
                <pre
                  // A scrollable region must take focus so keyboard users can scroll it.
                  // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable file view
                  tabIndex={0}
                  className="max-h-72 overflow-auto rounded-control bg-surface-2 p-2 font-mono text-mono"
                >
                  {markManagedLines(file.content).map((line, index) => (
                    <div
                      key={index}
                      data-managed={line.managed ? "true" : undefined}
                      className={line.managed ? "bg-accent/15 text-fg-primary" : undefined}
                    >
                      {line.text === "" ? " " : line.text}
                    </div>
                  ))}
                </pre>
                {file.truncated ? (
                  <p className="text-caption text-fg-tertiary">{t("config.truncated")}</p>
                ) : null}
              </>
            )
          }
        </QueryBoundary>
      )}
    </section>
  );
}
