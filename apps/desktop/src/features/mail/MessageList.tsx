import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { FOCUS_RING, cx } from "../../design/primitives/shared";
import { useFormatters } from "../../design/primitives/useFormatters";
import { VirtualList } from "../../design/primitives/VirtualList";
import type { MailSummaryDto } from "../../ipc/generated";
import { senderName } from "./model";

export const ROW_HEIGHT = 64;
const CLIP = "📎";
const MORE_KEY = "__more__";

type Item = { kind: "message"; message: MailSummaryDto } | { kind: "more" };

export interface MessageListProps {
  messages: readonly MailSummaryDto[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  /** More messages exist on the daemon than are loaded. */
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
}

/** Virtualized inbox rows: unread dot, sender, subject, time and the attachment icon. */
export function MessageList({
  messages,
  selectedId,
  onSelect,
  hasMore,
  loadingMore,
  onLoadMore,
}: MessageListProps) {
  const { t } = useTranslation("mail");
  const fmt = useFormatters();
  const items: Item[] = messages.map((message) => ({ kind: "message", message }));
  if (hasMore) items.push({ kind: "more" });

  return (
    <VirtualList
      items={items}
      label={t("list.label")}
      rowHeight={ROW_HEIGHT}
      overscan={6}
      initialHeight={600}
      getKey={(item) => (item.kind === "more" ? MORE_KEY : item.message.id)}
      renderRow={(item) => {
        if (item.kind === "more") {
          return (
            <div className="flex h-full items-center justify-center">
              <Button variant="ghost" loading={loadingMore} onClick={onLoadMore}>
                {t("list.more")}
              </Button>
            </div>
          );
        }
        const { message } = item;
        const selected = message.id === selectedId;
        return (
          <button
            type="button"
            data-mail-row={message.id}
            data-unread={message.read ? undefined : "true"}
            aria-current={selected ? "true" : undefined}
            onClick={() => {
              onSelect(message.id);
            }}
            className={cx(
              "flex h-full w-full items-center gap-2 border-b border-stroke px-3 text-left",
              selected ? "bg-surface-2" : "hover:bg-surface-2",
              FOCUS_RING,
            )}
          >
            <span
              aria-hidden="true"
              className={cx(
                "size-2 shrink-0 rounded-full",
                message.read ? "bg-transparent" : "bg-accent",
              )}
            />
            <span className="sr-only">{message.read ? t("row.read") : t("row.unread")}</span>
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="flex items-baseline gap-2">
                <span
                  className={cx(
                    "min-w-0 flex-1 truncate text-body",
                    message.read ? "text-fg-secondary" : "font-semibold text-fg-primary",
                  )}
                >
                  {senderName(message.from)}
                </span>
                <time
                  dateTime={message.received_at}
                  className="shrink-0 text-caption text-fg-tertiary"
                >
                  {fmt.relative(new Date(message.received_at))}
                </time>
              </span>
              <span className="flex items-center gap-1">
                <span
                  className={cx(
                    "min-w-0 flex-1 truncate text-callout",
                    message.read ? "text-fg-secondary" : "text-fg-primary",
                  )}
                >
                  {message.subject === "" ? t("row.noSubject") : message.subject}
                </span>
                {message.attachments > 0 ? (
                  <span
                    role="img"
                    aria-label={t("row.attachments", { count: message.attachments })}
                    className="shrink-0 text-caption"
                  >
                    {CLIP}
                  </span>
                ) : null}
              </span>
            </span>
          </button>
        );
      }}
    />
  );
}
