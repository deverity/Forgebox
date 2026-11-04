// Handlers of the Mail screen (M2-E3-T13): `mail.*` over a fake Mailpit mailbox. The mailbox
// exists while the data has a `mailpit` instance (otherwise the daemon's `not_found` with the
// install hint), mutations publish `mail.changed` like the daemon's MailHub, and the seeded
// messages cover the sandbox cases: an HTML body with a script, an event handler, a remote
// image, a `cid:` image and links, plus a text-only and an attachment-only message.
import { fakeError, type FakeCallContext, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import type { QaData } from "./data";

const ACK: T.Ack = { ok: true, message: null };
const MAILPIT = "mailpit";
const SMTP_PORT = 1025;

/** Address the seeded HTML message loads its remote image from (nothing listens there). */
export const REMOTE_IMAGE_URL = "http://127.0.0.1:9/tracking-pixel.png";
export const PHISHING_LINK = "https://example.test/reset?token=abc";

export const HTML_BODY = [
  "<html><head><style>h1{color:#0a5}</style></head><body>",
  "<h1>Reset your password</h1>",
  `<p onclick="window.__clicked=1">Click <a href="${PHISHING_LINK}" target="_blank">here</a>.</p>`,
  `<img alt="pixel" src="${REMOTE_IMAGE_URL}" width="1" height="1">`,
  '<img alt="logo" src="cid:logo@forgebox">',
  "<script>window.__ranScript = true; document.title = 'pwned';</script>",
  "</body></html>",
].join("");

export interface FakeMessage {
  summary: T.MailSummaryDto;
  headers: Record<string, string[]>;
  text: string | null;
  html: string | null;
  attachments: T.MailAttachmentDto[];
}

let counter = 0;

export interface MessagePatch {
  subject: string;
  /** Minutes since it arrived (default: one per message made). */
  ageMinutes?: number;
  summary?: Partial<T.MailSummaryDto>;
  headers?: Record<string, string[]>;
  text?: string | null;
  html?: string | null;
  attachments?: T.MailAttachmentDto[];
}

/** A message; unread by default (what a fresh capture is). */
export function message(patch: MessagePatch): FakeMessage {
  counter += 1;
  const id = `msg${String(counter).padStart(4, "0")}`;
  const at = new Date(Date.now() - (patch.ageMinutes ?? counter) * 60_000).toISOString();
  return {
    summary: {
      id,
      from: "Forgebox App <app@loja.test>",
      to: ["dev@loja.test"],
      subject: patch.subject,
      received_at: at,
      read: false,
      size_bytes: 1200,
      attachments: patch.attachments?.length ?? 0,
      ...patch.summary,
    },
    headers: patch.headers ?? {
      From: ["Forgebox App <app@loja.test>"],
      To: ["dev@loja.test"],
      Subject: [patch.subject],
    },
    text: patch.text ?? `Plain text of "${patch.subject}".`,
    html: patch.html ?? null,
    attachments: patch.attachments ?? [],
  };
}

/** The three seeded messages (newest first). */
export function seedMessages(): FakeMessage[] {
  return [
    message({ subject: "Reset your password", html: HTML_BODY, ageMinutes: 2 }),
    message({
      subject: "Invoice 2026-11",
      attachments: [
        {
          part_id: "1",
          file_name: "invoice.txt",
          content_type: "text/plain",
          size_bytes: 11,
          inline: false,
          content_id: null,
        },
      ],
      ageMinutes: 30,
      summary: { read: true },
    }),
    message({ subject: "Welcome aboard", ageMinutes: 90 }),
  ];
}

/** `count` unread messages, for the virtualization and paging cases. */
export function manyMessages(count: number): FakeMessage[] {
  return Array.from({ length: count }, (_, i) =>
    message({ subject: `Message ${String(i + 1)}`, ageMinutes: i + 1 }),
  );
}

export interface MailHandlers {
  handlers: FakeHandlers;
  /** The mailbox, newest first (tests inspect and extend it). */
  box: FakeMessage[];
  /** Adds a captured message and publishes `mail.changed` (`new_messages: 1`). */
  receive(ctx: Pick<FakeCallContext, "fake">, next: FakeMessage): void;
}

/** `mail.*` handlers over `box` (default: the seeded messages). */
export function mailHandlers(
  data: Pick<QaData, "instances">,
  box: FakeMessage[] = seedMessages(),
): MailHandlers {
  const instance = () => data.instances.find((i) => i.tool === MAILPIT);
  const need = (): T.InstanceDto => {
    const found = instance();
    if (found === undefined) {
      fakeError("not_found", "no mail instance (install one: forge tool install mailpit)");
    }
    if (found.state !== "running") {
      fakeError("conflict", "mailpit is stopped (forge start mailpit/default)");
    }
    return found;
  };
  const unread = () => box.filter((m) => !m.summary.read).length;
  const changed = (ctx: Pick<FakeCallContext, "fake">, instanceId: string, added: number) => {
    ctx.fake.publish("mail.changed", {
      v: 1,
      instance_id: instanceId,
      messages: box.length,
      unread: unread(),
      new_messages: added,
    } satisfies T.MailChangedPayload);
  };
  const find = (id: string) => {
    const found = box.find((m) => m.summary.id === id);
    if (found === undefined) fakeError("not_found", `message ${id} not found`);
    return found;
  };
  const handlers: FakeHandlers = {
    "mail.status": () => {
      const found = instance();
      if (found === undefined) {
        fakeError("not_found", "no mail instance (install one: forge tool install mailpit)");
      }
      return {
        running: found.state === "running",
        instance_id: found.id,
        smtp_port: SMTP_PORT,
        ui_url: "http://127.0.0.1:8025",
        messages: box.length,
        unread: unread(),
      };
    },
    "mail.list": (params) => {
      need();
      const query = (params.query ?? "").toLowerCase();
      const matches = box.filter(
        (m) =>
          (!params.unread || !m.summary.read) &&
          (query === "" ||
            `${m.summary.from} ${m.summary.subject} ${m.text ?? ""}`.toLowerCase().includes(query)),
      );
      const offset = params.offset ?? 0;
      const limit = params.limit ?? 50;
      return {
        messages: matches.slice(offset, offset + limit).map((m) => m.summary),
        total: matches.length,
        unread: unread(),
      };
    },
    "mail.show": (params, ctx) => {
      const instanceDto = need();
      const found = find(params.id);
      if (params.mark_read && !found.summary.read) {
        found.summary = { ...found.summary, read: true };
        changed(ctx, instanceDto.id, 0);
      }
      return {
        summary: found.summary,
        headers: found.headers,
        text: found.text,
        html: found.html,
        cc: [],
        bcc: [],
        reply_to: [],
        attachments: found.attachments,
      };
    },
    "mail.mark_read": (params, ctx) => {
      const instanceDto = need();
      for (const id of params.ids) {
        const found = find(id);
        found.summary = { ...found.summary, read: params.read ?? true };
      }
      changed(ctx, instanceDto.id, 0);
      return ACK;
    },
    "mail.delete": (params, ctx) => {
      const instanceDto = need();
      if (params.confirm !== true) {
        fakeError("invalid_input", "mail.delete needs confirm: true", "confirm_required");
      }
      for (const id of params.ids) box.splice(box.indexOf(find(id)), 1);
      changed(ctx, instanceDto.id, 0);
      return ACK;
    },
    "mail.clear": (params, ctx) => {
      const instanceDto = need();
      if (params.confirm !== true) {
        fakeError("invalid_input", "mail.clear needs confirm: true", "confirm_required");
      }
      box.length = 0;
      changed(ctx, instanceDto.id, 0);
      return ACK;
    },
    "mail.attachment": (params) => {
      need();
      const part = find(params.id).attachments.find((a) => a.part_id === params.part_id);
      if (part === undefined) fakeError("not_found", `part ${params.part_id} not found`);
      return {
        file_name: part.file_name,
        content_type: part.content_type,
        size_bytes: part.size_bytes,
        content_base64: btoa("hello world"),
      };
    },
    "mail.env": () => {
      need();
      return {
        vars: {
          MAIL_MAILER: "smtp",
          MAIL_HOST: "127.0.0.1",
          MAIL_PORT: String(SMTP_PORT),
          MAIL_USERNAME: "",
          MAIL_PASSWORD: "",
          MAIL_ENCRYPTION: "",
        },
      };
    },
    "mail.open": () => ACK,
  };
  return {
    handlers,
    box,
    receive: (ctx, next) => {
      box.unshift(next);
      changed(ctx, instance()?.id ?? "", 1);
    },
  };
}
