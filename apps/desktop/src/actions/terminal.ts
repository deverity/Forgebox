// Terminal actions (docs/07 §8, ADR-0040). `terminal.open` has no CLI twin through the table
// (its template is empty): the equivalent is `forge shell [dir]`, shown by the Terminal screen.
import type { TerminalOpenParams } from "../ipc/generated";
import { defineAction } from "./registry";

/** Opens a terminal session (the Terminal screen passes cols/rows measured by the fit addon). */
export const openTerminal = defineAction({
  id: "terminal.open",
  method: "terminal.open",
  toParams: (input: TerminalOpenParams | undefined) => input ?? {},
});

/** Closes a terminal session on the daemon (kills the process if it still runs). */
export const closeTerminal = defineAction({
  id: "terminal.close",
  method: "terminal.close",
  toParams: (input: { id: string }) => ({ id: input.id }),
});
