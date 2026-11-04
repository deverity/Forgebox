// Mail actions (docs/07 section 5.7). Registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the E4-T5 parity gate see every mutation of the Mail screen.
import { defineAction } from "./registry";

/** `forge mail read <id>... [--unread]`. */
export const markMailRead = defineAction<"mail.mark_read", { ids: string[]; read: boolean }>({
  id: "mail.mark_read",
  method: "mail.mark_read",
  toParams: ({ ids, read }) => ({ ids, read }),
});

/** `forge mail delete <id>...`: the daemon requires `confirm: true` (the undo toast is the GUI's). */
export const deleteMail = defineAction<"mail.delete", string[]>({
  id: "mail.delete",
  method: "mail.delete",
  toParams: (ids) => ({ ids, confirm: true }),
});

/** `forge mail clear --yes`: empties the mailbox; the daemon requires `confirm: true`. */
export const clearMail = defineAction({
  id: "mail.clear",
  method: "mail.clear",
  toParams: () => ({ confirm: true }),
});

/** "Abrir no Mailpit": the daemon opens the web UI on its machine (`local_only`). */
export const openMailpit = defineAction({
  id: "mail.open",
  method: "mail.open",
  toParams: () => ({}),
});
