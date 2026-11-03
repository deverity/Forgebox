// Terminal actions (docs/07 §8). `terminal.open` has no CLI twin (its template is empty).
import { defineAction } from "./registry";

/** Opens a terminal in the root (the global shortcuts and the palette). */
export const openTerminal = defineAction({
  id: "terminal.open",
  method: "terminal.open",
  toParams: () => ({}),
});
