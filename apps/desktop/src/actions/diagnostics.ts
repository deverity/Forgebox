// The fixes the Diagnostics screen may run (docs/07 §5.12). A check names its fix with
// `fix_method` (API 1.1, ADR-0040); the screen runs it only when the method is in THIS table,
// so a daemon that suggests anything else (`daemon.stop`...) only gets "Copiar comando".
import type { CaInstallParams } from "../ipc/generated";
import { defineAction, type Action } from "./registry";

/** Turns DNS on (NRPT rule or `hosts` fallback): `forge dns enable`. */
export const enableDns = defineAction({
  id: "diagnostics.dns_enable",
  method: "net.dns_enable",
  toParams: () => ({}),
});

/** Trusts the local CA for the user or the machine: `forge tls ca install --scope user`. */
export const installCa = defineAction<"tls.ca_install", CaInstallParams>({
  id: "diagnostics.ca_install",
  method: "tls.ca_install",
  toParams: (params) => params,
});

/** Adds the Forgebox entries to the user PATH: `forge path add`. */
export const addToPath = defineAction({
  id: "diagnostics.path_add",
  method: "shell.path_add",
  toParams: () => ({}),
});

/** A fix the client can run: the action and how `fix_params` becomes its input. */
export interface FixEntry {
  action: Action<never, never>;
  /** Builds the action input from the check's `fix_params` (absent or malformed: a default). */
  input: (params: unknown) => unknown;
}

const SCOPES: ReadonlySet<string> = new Set(["current_user", "local_machine"]);

function scopeOf(params: unknown): CaInstallParams {
  const scope =
    typeof params === "object" && params !== null && "scope" in params ? params.scope : undefined;
  return {
    scope:
      typeof scope === "string" && SCOPES.has(scope)
        ? (scope as CaInstallParams["scope"])
        : "current_user",
  };
}

/** The client allowlist (docs/07 §5.12): `fix_method` -> what runs. */
export const FIX_ALLOWLIST: Readonly<Record<string, FixEntry>> = {
  "net.dns_enable": { action: enableDns as never, input: () => undefined },
  "tls.ca_install": { action: installCa as never, input: scopeOf },
  "shell.path_add": { action: addToPath as never, input: () => undefined },
};
