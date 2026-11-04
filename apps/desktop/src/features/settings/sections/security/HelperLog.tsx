import { useTranslation } from "react-i18next";
import { Button } from "../../../../design/primitives/Button";
import { ErrorNotice } from "../../../../design/primitives/ErrorNotice";
import { useCapabilityReason } from "../../../../app/shell/useErrorText";
import { useCapability, useForgeQuery } from "../../../../state";
import { RowFrame } from "../../RowFrame";
import { parseHelperLine } from "./model";

const METHOD_FS_TAIL = "fs.tail";
const TID_HELPER_LOG = "helper-log";
const TID_HELPER_LOG_DENIED = "helper-log-denied";
const TID_HELPER_LOG_EMPTY = "helper-log-empty";

/** Path of the helper log, relative to the root (docs/08 section 2; `fs.tail` is root-relative). */
export const HELPER_LOG = "logs/helper.log";
const TAIL_LINES = 100;

/**
 * The last lines of `logs/helper.log` (everything the elevated helper did: JSON lines with
 * op, result and message). `fs.tail` needs the Admin role, so for a lower role the card says
 * why instead of failing.
 */
export function HelperLog() {
  const { t } = useTranslation("settings");
  const capability = useCapability("fs.tail");
  const reason = useCapabilityReason(capability);
  const tail = useForgeQuery("fs.tail", { path: HELPER_LOG, lines: TAIL_LINES });
  const lines = (tail.data?.lines ?? []).map(parseHelperLine);
  return (
    <RowFrame
      id="helperLog"
      label={t("security.helper.label")}
      description={t("security.helper.description")}
      caption={HELPER_LOG}
    >
      <div className="flex w-full min-w-72 flex-col gap-2">
        {reason !== null ? (
          <p data-testid={TID_HELPER_LOG_DENIED} className="text-callout text-fg-secondary">
            {reason}
          </p>
        ) : null}
        {tail.error !== null && reason === null ? (
          <ErrorNotice
            error={tail.error}
            context={{ method: METHOD_FS_TAIL }}
            onRetry={() => {
              void tail.refetch();
            }}
          />
        ) : null}
        {reason === null && tail.data !== undefined && lines.length === 0 ? (
          <p data-testid={TID_HELPER_LOG_EMPTY} className="text-callout text-fg-secondary">
            {t("security.helper.empty")}
          </p>
        ) : null}
        {lines.length === 0 ? null : (
          <ol
            data-testid={TID_HELPER_LOG}
            aria-label={t("security.helper.label")}
            className="max-h-64 overflow-auto rounded-control border border-stroke bg-surface p-2 font-mono text-mono"
          >
            {lines.map((line, index) => (
              <li
                key={`${String(index)}-${line.raw}`}
                data-ok={line.entry === null ? undefined : line.entry.ok}
                className="whitespace-pre-wrap [overflow-wrap:anywhere]"
              >
                {line.entry === null
                  ? line.raw
                  : `${line.entry.ts} ${line.entry.op} ${line.entry.ok ? t("security.helper.ok") : t("security.helper.failed")} ${line.entry.message}`}
              </li>
            ))}
          </ol>
        )}
        {reason === null ? (
          <div>
            <Button
              variant="ghost"
              loading={tail.isFetching}
              loadingLabel={t("security.helper.refreshing")}
              onClick={() => {
                void tail.refetch();
              }}
            >
              {t("security.helper.refresh")}
            </Button>
          </div>
        ) : null}
      </div>
    </RowFrame>
  );
}
