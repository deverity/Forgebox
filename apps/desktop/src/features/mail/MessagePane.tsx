import { Tabs } from "radix-ui";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { markMailRead } from "../../actions/mail";
import { runAction } from "../../actions/registry";
import { useErrorMessage } from "../../app/shell/useErrorText";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Button } from "../../design/primitives/Button";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { FOCUS_RING } from "../../design/primitives/shared";
import { Skeleton } from "../../design/primitives/Skeleton";
import { toast } from "../../design/primitives/toastStore";
import { Tooltip } from "../../design/primitives/Tooltip";
import { useFormatters } from "../../design/primitives/useFormatters";
import { newIntent } from "../../ipc/api";
import { renderCli } from "../../ipc/cli";
import type * as T from "../../ipc/generated";
import { saveAttachment } from "../../ipc/native";
import { useCapability, useForgeApi, useRuntime } from "../../state";
import { useMailMessage } from "../../state/mail";
import { HtmlPreview } from "./HtmlPreview";
import { headerLines } from "./model";

const TABS = ["html", "text", "headers", "attachments"] as const;
type MailTab = (typeof TABS)[number];
const [HTML_TAB, TEXT_TAB, HEADERS_TAB, ATTACHMENTS_TAB] = TABS;
const ARROW = " → ";
const SEPARATOR = " · ";

const TAB_CLASS = `-mb-px border-b-2 border-transparent shrink-0 px-2.5 py-2 text-callout text-fg-secondary hover:text-fg-primary ${FOCUS_RING} data-[state=active]:border-accent data-[state=active]:text-fg-primary`;

export interface MessagePaneProps {
  id: string | null;
  onDelete: (message: T.MailSummaryDto) => void;
}

function AttachmentRow({ messageId, part }: { messageId: string; part: T.MailAttachmentDto }) {
  const { t } = useTranslation("mail");
  const api = useForgeApi();
  const fmt = useFormatters();
  const errorMessage = useErrorMessage();
  const [busy, setBusy] = useState(false);

  const download = () => {
    setBusy(true);
    api.call("mail.attachment", { id: messageId, part_id: part.part_id }).then(
      (content) =>
        saveAttachment(content.file_name, content.content_base64, content.content_type).then(
          (result) => {
            if (result.status === "saved") {
              toast.show({
                kind: "success",
                message: t("attachments.saved", { name: content.file_name }),
              });
            }
            setBusy(false);
          },
        ),
      (error: unknown) => {
        toast.show({ kind: "error", message: errorMessage(error) });
        setBusy(false);
      },
    );
  };

  return (
    <li className="flex items-center gap-3 border-b border-stroke py-2">
      <span className="min-w-0 flex-1">
        <span className="block truncate text-body">{part.file_name}</span>
        <span className="block text-caption text-fg-tertiary">
          {part.content_type}
          {SEPARATOR}
          {fmt.bytes(part.size_bytes)}
          {part.inline ? `${SEPARATOR}${t("attachments.inline")}` : ""}
        </span>
      </span>
      <Button
        variant="secondary"
        loading={busy}
        aria-label={t("attachments.downloadNamed", { name: part.file_name })}
        onClick={download}
      >
        {t("attachments.download")}
      </Button>
    </li>
  );
}

/** The selected message: header, actions and the HTML / Texto / Headers / Anexos tabs. */
export function MessagePane({ id, onDelete }: MessagePaneProps) {
  const { t } = useTranslation("mail");
  const fmt = useFormatters();
  const runtime = useRuntime();
  const message = useMailMessage(id);
  const canMark = useCapability("mail.mark_read");
  const [chosenTab, setTab] = useState<MailTab | null>(null);
  const [remoteAllowed, setRemoteAllowed] = useState<ReadonlySet<string>>(new Set());
  const autoMarked = useRef(new Set<string>());
  const dto = message.data;

  // Opening an unread message reads it, once per message and session; "marcar como não lida"
  // afterwards sticks.
  const summary = dto?.summary;
  useEffect(() => {
    if (summary === undefined || summary.read || !canMark.available) return;
    if (autoMarked.current.has(summary.id)) return;
    autoMarked.current.add(summary.id);
    runAction(
      runtime.queue,
      markMailRead,
      { ids: [summary.id], read: true },
      { intent: newIntent() },
    ).catch(() => undefined);
  }, [summary, canMark.available, runtime.queue]);

  // Each message starts on its best tab (the parent remounts this pane per message).
  const hasHtml = dto?.html !== null && dto?.html !== undefined;
  const tab: MailTab = chosenTab ?? (hasHtml ? HTML_TAB : TEXT_TAB);

  if (id === null) {
    return <EmptyState title={t("pane.none")} description={t("pane.noneHint")} />;
  }
  if (message.isPending) {
    return (
      <div className="flex flex-col gap-3 p-4" aria-busy="true">
        <Skeleton height={28} />
        <Skeleton height={16} />
        <Skeleton height={240} />
      </div>
    );
  }
  if (message.isError || dto === undefined) {
    return (
      <div className="p-4">
        <ErrorNotice error={message.error} onRetry={() => void message.refetch()} />
      </div>
    );
  }

  const allowRemote = remoteAllowed.has(dto.summary.id);
  const read = dto.summary.read;
  const deleteCli = renderCli("mail.delete", { ids: [dto.summary.id], confirm: true });
  const attachments = dto.attachments ?? [];

  return (
    <div data-testid="mail-pane" className="flex min-h-0 flex-1 flex-col gap-3 p-4">
      <header className="flex flex-col gap-1">
        <div className="flex items-start gap-2">
          <h2 className="min-w-0 flex-1 text-title-2 break-words">
            {dto.summary.subject === "" ? t("row.noSubject") : dto.summary.subject}
          </h2>
          <ActionButton
            action={markMailRead}
            input={{ ids: [dto.summary.id], read: !read }}
            variant="secondary"
            successLabel={t("pane.done")}
          >
            {read ? t("pane.markUnread") : t("pane.markRead")}
          </ActionButton>
          <Tooltip
            content={t("pane.delete")}
            {...(deleteCli === null ? {} : { command: deleteCli })}
          >
            <Button
              variant="destructive"
              onClick={() => {
                onDelete(dto.summary);
              }}
            >
              {t("pane.delete")}
            </Button>
          </Tooltip>
        </div>
        <p className="text-callout text-fg-secondary">
          <span className="text-fg-primary">{dto.summary.from}</span>
          {ARROW}
          {dto.summary.to.join(", ")}
        </p>
        <p className="text-caption text-fg-tertiary">
          {fmt.date(new Date(dto.summary.received_at))}
        </p>
      </header>
      <Tabs.Root
        value={tab}
        onValueChange={(value) => {
          setTab(value as MailTab);
        }}
        className="flex min-h-0 flex-1 flex-col"
      >
        <Tabs.List aria-label={t("tabs.label")} className="flex border-b border-stroke">
          {TABS.map((name) => (
            <Tabs.Trigger key={name} value={name} className={TAB_CLASS}>
              {name === "attachments"
                ? t("tabs.attachmentsCount", { count: attachments.length })
                : t(`tabs.${name}`)}
            </Tabs.Trigger>
          ))}
        </Tabs.List>
        <Tabs.Content
          value={HTML_TAB}
          className={`flex min-h-0 flex-1 flex-col pt-3 ${FOCUS_RING}`}
        >
          {dto.html === null ? (
            <p className="text-callout text-fg-secondary">{t("html.none")}</p>
          ) : (
            <HtmlPreview
              html={dto.html}
              allowRemote={allowRemote}
              onAllowRemote={(allow) => {
                setRemoteAllowed((current) => {
                  const next = new Set(current);
                  if (allow) next.add(dto.summary.id);
                  else next.delete(dto.summary.id);
                  return next;
                });
              }}
              onShowText={() => {
                setTab(TEXT_TAB);
              }}
            />
          )}
        </Tabs.Content>
        <Tabs.Content
          value={TEXT_TAB}
          className={`min-h-0 flex-1 overflow-auto pt-3 ${FOCUS_RING}`}
        >
          {dto.text === null || dto.text === "" ? (
            <p className="text-callout text-fg-secondary">{t("text.none")}</p>
          ) : (
            <pre
              data-testid="mail-text"
              className="font-mono text-mono whitespace-pre-wrap text-fg-primary"
            >
              {dto.text}
            </pre>
          )}
        </Tabs.Content>
        <Tabs.Content
          value={HEADERS_TAB}
          className={`min-h-0 flex-1 overflow-auto pt-3 ${FOCUS_RING}`}
        >
          <pre
            data-testid="mail-headers"
            className="font-mono text-mono break-all whitespace-pre-wrap"
          >
            {headerLines(dto.headers).join("\n")}
          </pre>
        </Tabs.Content>
        <Tabs.Content
          value={ATTACHMENTS_TAB}
          className={`min-h-0 flex-1 overflow-auto pt-3 ${FOCUS_RING}`}
        >
          {attachments.length === 0 ? (
            <p className="text-callout text-fg-secondary">{t("attachments.none")}</p>
          ) : (
            <ul data-testid="mail-attachments">
              {attachments.map((part) => (
                <AttachmentRow key={part.part_id} messageId={dto.summary.id} part={part} />
              ))}
            </ul>
          )}
        </Tabs.Content>
      </Tabs.Root>
    </div>
  );
}
