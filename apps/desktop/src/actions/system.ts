// Actions of the Settings screen (docs/07 §5.11): `forge config set` and the follow-ups a change
// can ask for through `ConfigValueDto.requires` (ADR-0040).
import { defineAction } from "./registry";

/** `forge config set <key> <value>`: writes `forgebox.toml` (the daemon validates it). */
export const setConfig = defineAction<"system.config_set", { key: string; value: unknown }>({
  id: "settings.config_set",
  method: "system.config_set",
  toParams: ({ key, value }) => ({ key, value }),
});

/** `forge daemon restart`: a `requires: ["daemon.restart"]` follow-up. */
export const restartDaemon = defineAction({
  id: "settings.daemon_restart",
  method: "daemon.restart",
  toParams: () => ({}),
});

/** `forge tls ca rotate`: a `requires: ["tls.ca_rotate"]` follow-up (TLD change). Destructive:
 * the screen asks first, then this sends `confirm: true`. */
export const rotateCa = defineAction({
  id: "settings.ca_rotate",
  method: "tls.ca_rotate",
  toParams: () => ({ confirm: true }),
});
