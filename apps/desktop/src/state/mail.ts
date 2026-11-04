// Mail state (M2-E3-T13): the mailbox counts that feed the sidebar badge and the tray, and the
// queries of the Mail screen. Nothing here polls: `mail.changed` events carry the counts (the
// reducer writes them to `stores.mail`) and invalidate `mail.status`/`mail.list` once per burst
// (EVENT_INVALIDATIONS), so the screen refetches only when the daemon says something changed.
import { keepPreviousData, useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useStore } from "zustand";
import type { ForgeApiError } from "../ipc/errors";
import type * as T from "../ipc/generated";
import { forgeQuery } from "./queries";
import { useCapability, useForgeApi, useForgeQuery, useRuntime } from "./react";

/** The numbers the badge and the screen header show. */
export interface MailCounts {
  messages: number;
  unread: number;
}

/**
 * Mailbox counts: the newest of the last `mail.changed` and the last `mail.status` answer
 * (a mark-read the GUI made refetches `mail.status`; an event from another client arrives
 * first). `null` while neither is known.
 */
export function useMailCounts(): MailCounts | null {
  const capability = useCapability("mail.status");
  const status = useForgeQuery("mail.status", {}, { enabled: capability.available });
  const event = useStore(useRuntime().stores.mail, (state) => state.counts);
  if (!capability.available) return null;
  const fromStatus =
    status.data === undefined
      ? null
      : { messages: status.data.messages, unread: status.data.unread };
  if (event !== null && (fromStatus === null || event.at >= status.dataUpdatedAt)) {
    return { messages: event.messages, unread: event.unread };
  }
  return fromStatus;
}

/**
 * Unread messages for the badge. `null` hides it: `mail.status` is not implemented (or not
 * allowed) on this daemon, there is no mailbox, or nothing is unread.
 */
export function useMailUnread(): number | null {
  const unread = useMailCounts()?.unread ?? 0;
  return unread > 0 ? unread : null;
}

/** Messages fetched per page (`limit` grows by this much with "Carregar mais"). */
export const MAIL_PAGE_SIZE = 50;

export interface MailListQuery {
  query: string;
  unreadOnly: boolean;
  /** How many pages of `MAIL_PAGE_SIZE` to fetch. */
  pages: number;
}

/** `mail.list` for the screen; keeps the previous page on screen while a new one loads. */
export function useMailList(options: MailListQuery): UseQueryResult<T.MailListDto, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("mail.list");
  const params: T.MailListParams = {
    unread: options.unreadOnly,
    limit: options.pages * MAIL_PAGE_SIZE,
    ...(options.query.trim() === "" ? {} : { query: options.query.trim() }),
  };
  return useQuery({
    ...forgeQuery(api, "mail.list", params),
    enabled: capability.available,
    placeholderData: keepPreviousData,
  });
}

/**
 * One message with its headers, text, HTML and attachments. It never marks the message read
 * itself (`mark_read: false`): the screen does that once per opening through `mail.mark_read`,
 * so refetching after "marcar como não lida" cannot undo the user's choice.
 */
export function useMailMessage(id: string | null): UseQueryResult<T.MailMessageDto, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability("mail.show");
  return useQuery({
    ...forgeQuery(api, "mail.show", { id: id ?? "", mark_read: false }),
    enabled: capability.available && id !== null,
  });
}
