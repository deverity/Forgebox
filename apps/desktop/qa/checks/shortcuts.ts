// Docs/07 section 8: the in-app shortcuts. `effect` is the selector that must become visible
// after the keys; null means the shortcut is not implemented yet (the report lists it as
// skipped with the task that delivers it). The PR that implements one flips its row on.
import type { ShortcutCheck } from "./lib";

export const SECTION_8: readonly ShortcutCheck[] = [
  { keys: "Control+k", label: "command palette", effect: null, pending: "M2-E1-T14" },
  { keys: "Control+Shift+s", label: "start all", effect: null, pending: "M2-E1-T14" },
  { keys: "Control+Shift+x", label: "stop all", effect: null, pending: "M2-E1-T14" },
  { keys: "Control+n", label: "new site", effect: null, pending: "M2-E1-T14" },
  { keys: "Control+l", label: "logs", effect: null, pending: "M2-E1-T14" },
  { keys: "Control+,", label: "settings", effect: null, pending: "M2-E1-T14" },
];

/** The shortcut of the self-test page: Ctrl+J shows the panel in the `ok` case only. */
export const SELFTEST_SHORTCUT: ShortcutCheck = {
  keys: "Control+j",
  label: "self-test panel",
  effect: "#selftest-panel",
};
