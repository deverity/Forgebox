// The contexts the selector offers (docs/13 section 5). Data-driven so M6 only appends rows:
// a remote context brings its own accent (applied to the window by `AppShell`) so Start/Stop
// is never sent to the wrong server by mistake.
import { LOCAL_CONTEXT, type ContextId } from "../../ipc/bridge";

export interface ContextOption {
  id: ContextId;
  /** `shell` i18n key of the label; remote profiles will carry a literal `name` instead. */
  labelKey: "context.local" | "context.addRemote";
  name?: string;
  /** Window accent while this context is active (`#RRGGBB`); `null` keeps the system accent. */
  accent: string | null;
  disabled: boolean;
}

export const ADD_REMOTE_ID = "__add_remote__";

export const CONTEXT_OPTIONS: readonly ContextOption[] = [
  { id: LOCAL_CONTEXT, labelKey: "context.local", accent: null, disabled: false },
  { id: ADD_REMOTE_ID, labelKey: "context.addRemote", accent: null, disabled: true },
];

export function accentOf(id: ContextId, options: readonly ContextOption[] = CONTEXT_OPTIONS) {
  return options.find((o) => o.id === id)?.accent ?? null;
}
