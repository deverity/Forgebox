import { Link } from "@tanstack/react-router";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useStore } from "zustand";
import { deleteMail, openMailpit } from "../../actions/mail";
import { runAction } from "../../actions/registry";
import { restartInstance, startInstance } from "../../actions/services";
import { ActionButton } from "../../design/primitives/ActionButton";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { EmptyState } from "../../design/primitives/EmptyState";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { Skeleton } from "../../design/primitives/Skeleton";
import { toast } from "../../design/primitives/toastStore";
import { newIntent } from "../../ipc/api";
import type * as T from "../../ipc/generated";
import {
  useCapability,
  useConnectionStatus,
  useForgeQuery,
  useHello,
  useInstance,
  useInstances,
  useRuntime,
} from "../../state";
import { useMailCounts, useMailList } from "../../state/mail";
import { targetOf } from "../services/model";
import { ClearDialog } from "./ClearDialog";
import { CredentialsPanel } from "./CredentialsPanel";
import { MessageList } from "./MessageList";
import { MessagePane } from "./MessagePane";
import { DELETE_UNDO_MS, cancelDelete, pendingDeletes, scheduleDelete } from "./pendingDeletes";

const SEARCH_DEBOUNCE_MS = 250;
const INSTALL_COMMAND = "forge tool install mailpit";
const SEPARATOR = " · ";
const CATALOG_TOOL_ROUTE = "/catalog/$id";
const MAILPIT_TOOL = "mailpit";
const NO_MESSAGE = "none";
const FILTER_ALL = "all";
const FILTER_UNREAD = "unread";

function useDebounced<V>(value: V, ms: number): V {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => {
      setDebounced(value);
    }, ms);
    return () => {
      clearTimeout(timer);
    };
  }, [value, ms]);
  return debounced;
}

const SEARCH_CLASS =
  "h-8 w-64 rounded-control border border-stroke-strong bg-surface px-3 text-body text-fg-primary placeholder:text-fg-tertiary outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

/** The mailbox: toolbar, list and preview. Rendered once `mail.status` says Mailpit runs. */
function Inbox({ status }: { status: T.MailStatusDto }) {
  const { t } = useTranslation("mail");
  const runtime = useRuntime();
  const hello = useHello();
  const counts = useMailCounts();
  const instance = useInstance(status.instance_id ?? "");
  const [search, setSearch] = useState("");
  const query = useDebounced(search, SEARCH_DEBOUNCE_MS);
  const [unreadOnly, setUnreadOnly] = useState(false);
  const [pages, setPages] = useState(1);
  const [selected, setSelected] = useState<string | null>(null);
  const [showCredentials, setShowCredentials] = useState(false);
  const [clearing, setClearing] = useState(false);
  const list = useMailList({ query, unreadOnly, pages });
  const pending = useStore(pendingDeletes, (state) => state.ids);

  const messages = useMemo(
    () => (list.data?.messages ?? []).filter((message) => !pending.has(message.id)),
    [list.data, pending],
  );
  const selectedId =
    selected !== null && messages.some((message) => message.id === selected) ? selected : null;
  const total = list.data?.total ?? 0;
  const filtered = query.trim() !== "" || unreadOnly;
  const empty = list.data !== undefined && messages.length === 0 && !filtered && pending.size === 0;
  const local = hello?.local === true;
  const target = instance === undefined ? null : targetOf(instance);

  const onDelete = (message: T.MailSummaryDto) => {
    const index = messages.findIndex((m) => m.id === message.id);
    const neighbour = messages[index + 1] ?? messages[index - 1];
    setSelected(neighbour?.id ?? null);
    scheduleDelete(
      message.id,
      async () => {
        await runAction(runtime.queue, deleteMail, [message.id], { intent: newIntent() });
      },
      (ok) => {
        if (!ok) toast.show({ kind: "error", message: t("delete.failed") });
      },
    );
    toast.show({
      message: t("delete.toast"),
      durationMs: DELETE_UNDO_MS,
      undo: {
        label: t("delete.undo"),
        onUndo: () => {
          if (cancelDelete(message.id)) setSelected(message.id);
        },
      },
    });
  };

  const header = (
    <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
      <h1 className="text-title-1">{t("title")}</h1>
      <span className="text-callout text-fg-secondary" aria-live="polite" data-testid="mail-count">
        {t("count", { count: counts?.messages ?? status.messages })}
        {SEPARATOR}
        {t("unreadCount", { count: counts?.unread ?? status.unread })}
      </span>
      <div className="ml-auto flex flex-wrap items-center gap-2">
        <input
          type="search"
          aria-label={t("search.label")}
          placeholder={t("search.placeholder")}
          value={search}
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => {
            setSearch(event.target.value);
            setPages(1);
          }}
          className={SEARCH_CLASS}
        />
        <SegmentedControl
          label={t("filter.label")}
          value={unreadOnly ? FILTER_UNREAD : FILTER_ALL}
          onValueChange={(value) => {
            setUnreadOnly(value === FILTER_UNREAD);
            setPages(1);
          }}
          options={[
            { value: FILTER_ALL, label: t("filter.all") },
            { value: FILTER_UNREAD, label: t("filter.unread") },
          ]}
        />
        <Button
          variant="secondary"
          aria-expanded={showCredentials}
          onClick={() => {
            setShowCredentials((open) => !open);
          }}
        >
          {t("credentials.toggle")}
        </Button>
        {local ? (
          <ActionButton action={openMailpit} variant="secondary" successLabel={t("openedDone")}>
            {t("open")}
          </ActionButton>
        ) : null}
        <Button
          variant="destructive"
          disabled={(counts?.messages ?? status.messages) === 0}
          onClick={() => {
            setClearing(true);
          }}
        >
          {t("clear.button")}
        </Button>
      </div>
    </div>
  );

  let body;
  if (list.isPending) {
    body = (
      <div className="flex flex-col gap-2 p-4" aria-busy="true">
        <Skeleton height={56} />
        <Skeleton height={56} />
        <Skeleton height={56} />
      </div>
    );
  } else if (list.isError && list.data === undefined) {
    body = (
      <MailboxError
        error={list.error}
        target={target}
        onRetry={() => void list.refetch()}
        title={t("error.title")}
      />
    );
  } else if (empty) {
    body = (
      <div className="mx-auto flex w-full max-w-xl flex-col gap-4 p-6">
        <EmptyState title={t("empty.title")} description={t("empty.description")} />
        <CredentialsPanel />
      </div>
    );
  } else {
    body = (
      <div className="flex min-h-0 flex-1">
        <div
          data-testid="mail-list"
          className="flex w-[360px] shrink-0 flex-col border-r border-stroke"
        >
          {messages.length === 0 ? (
            <EmptyState title={t("list.noMatchTitle")} description={t("list.noMatchDescription")} />
          ) : (
            <MessageList
              messages={messages}
              selectedId={selectedId}
              onSelect={setSelected}
              hasMore={messages.length < total}
              loadingMore={list.isFetching}
              onLoadMore={() => {
                setPages((p) => p + 1);
              }}
            />
          )}
        </div>
        <div className="flex min-w-0 flex-1 flex-col overflow-y-auto">
          <MessagePane key={selectedId ?? NO_MESSAGE} id={selectedId} onDelete={onDelete} />
        </div>
      </div>
    );
  }

  return (
    <div data-testid="mail-inbox" className="flex min-h-0 flex-1 flex-col">
      {header}
      {showCredentials && !empty ? (
        <div className="px-4 pb-3">
          <CredentialsPanel />
        </div>
      ) : null}
      <div className="flex min-h-0 flex-1 flex-col">{body}</div>
      <ClearDialog
        open={clearing}
        count={counts?.messages ?? status.messages}
        onClose={() => {
          setClearing(false);
        }}
        onCleared={() => {
          setClearing(false);
          setSelected(null);
        }}
      />
    </div>
  );
}

/** "Mailpit não respondeu": the cause, a retry and, when the instance is known, a restart. */
function MailboxError({
  error,
  target,
  onRetry,
  title,
}: {
  error: unknown;
  target: string | null;
  onRetry: () => void;
  title: string;
}) {
  const { t } = useTranslation("mail");
  return (
    <div data-testid="mail-error" className="mx-auto flex max-w-lg flex-col gap-3 p-6">
      <h2 className="text-headline">{title}</h2>
      <p className="text-callout text-fg-secondary">{t("error.description")}</p>
      <ErrorNotice error={error as never} onRetry={onRetry} />
      {target !== null ? (
        <div className="flex">
          <ActionButton
            action={restartInstance}
            input={target}
            variant="primary"
            successLabel={t("error.restarted")}
          >
            {t("error.restart")}
          </ActionButton>
        </div>
      ) : null}
    </div>
  );
}

/** /mail (docs/07 section 5.7). */
export function MailPage() {
  const { t } = useTranslation("mail");
  const connection = useConnectionStatus();
  const capability = useCapability("mail.status");
  const status = useForgeQuery("mail.status", {}, { enabled: capability.available });
  const known = useInstance(status.data?.instance_id ?? "");
  const instances = useInstances();
  // Last good answer first; otherwise the Mailpit instance from the store, so a hung
  // Mailpit (timeout/conflict) still offers Restart.
  const instance =
    known ?? Object.values(instances).find((candidate) => candidate.tool === MAILPIT_TOOL);
  const target = instance === undefined ? null : targetOf(instance);

  let body;
  if (!capability.available) {
    body = <EmptyState title={t("unavailable.title")} description={t("unavailable.description")} />;
  } else if (status.isPending) {
    body =
      connection === "connected" ? (
        <div className="flex flex-col gap-2 p-4" aria-busy="true">
          <Skeleton height={56} />
          <Skeleton height={56} />
          <Skeleton height={56} />
        </div>
      ) : (
        <EmptyState title={t("offline.title")} description={t("offline.description")} />
      );
  } else if (status.isError && status.error.code === "not_found" && status.data === undefined) {
    body = (
      <div data-testid="mail-no-tool" className="flex flex-col items-center gap-3 p-8">
        <EmptyState
          title={t("noTool.title")}
          description={t("noTool.description")}
          action={
            <Link to={CATALOG_TOOL_ROUTE} params={{ id: MAILPIT_TOOL }}>
              <Button variant="primary">{t("noTool.action")}</Button>
            </Link>
          }
        />
        <div className="w-full max-w-md">
          <CliCommand command={INSTALL_COMMAND} />
        </div>
      </div>
    );
  } else if (status.isError && status.data === undefined) {
    body = (
      <MailboxError
        error={status.error}
        target={target}
        onRetry={() => void status.refetch()}
        title={t("error.title")}
      />
    );
  } else if (!status.data.running) {
    body = (
      <div data-testid="mail-stopped" className="flex flex-col items-center gap-3 p-8">
        <EmptyState
          title={t("stopped.title")}
          description={t("stopped.description")}
          {...(target === null
            ? {}
            : {
                action: (
                  <ActionButton action={startInstance} input={target} variant="primary">
                    {t("stopped.start")}
                  </ActionButton>
                ),
              })}
        />
      </div>
    );
  } else {
    body = <Inbox status={status.data} />;
  }

  return (
    <div data-testid="page-mail" className="flex min-h-0 flex-1 flex-col">
      {status.data?.running === true ? null : (
        <h1 className="px-4 pt-4 pb-3 text-title-1">{t("title")}</h1>
      )}
      {body}
    </div>
  );
}
