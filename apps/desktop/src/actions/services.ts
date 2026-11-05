// Service actions (docs/07 §1.7). Mutating methods are registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the E4-T5 parity gate can see every one of them.
import type { ServiceUpdateParams } from "../ipc/generated";
import { defineAction } from "./registry";

/** "Start all": `services.start` with no targets (the daemon reads that as all). */
export const startAllServices = defineAction({
  id: "services.start",
  method: "services.start",
  toParams: () => ({}),
});

/** "Stop all": `services.stop` with no targets (all). */
export const stopAllServices = defineAction({
  id: "services.stop",
  method: "services.stop",
  toParams: () => ({}),
});

/** Starts one instance, named by its target (`mysql/default`). */
export const startInstance = defineAction<"services.start", string>({
  id: "services.start_instance",
  method: "services.start",
  toParams: (target) => ({ targets: [target] }),
});

/** Stops one instance, named by its target (`mysql/default`). */
export const stopInstance = defineAction<"services.stop", string>({
  id: "services.stop_instance",
  method: "services.stop",
  toParams: (target) => ({ targets: [target] }),
});

/** Restarts one instance, named by its target. */
export const restartInstance = defineAction<"services.restart", string>({
  id: "services.restart_instance",
  method: "services.restart",
  toParams: (target) => ({ targets: [target] }),
});

/** Moves the main port of an instance (`forge service set mysql/default ports.main=3307`). */
export const setServicePort = defineAction<"services.set", { target: string; port: number }>({
  id: "services.set_port",
  method: "services.set",
  // `services.set` settings are strings in the contract.
  toParams: ({ target, port }) => ({ target, settings: { "ports.main": String(port) } }),
});

/** What the "new instance" form collects. */
export interface CreateInstanceInput {
  /** Tool spec (`mysql@8.4.3`). */
  tool: string;
  name: string;
  port: number | null;
  autostart: boolean;
  /** Empty: the manifest's default folder. */
  dataDir: string;
}

/** Creates an instance (`forge service create mysql@8.4.3 legacy --port 3307`). */
export const createInstance = defineAction<"services.create", CreateInstanceInput>({
  id: "services.create_instance",
  method: "services.create",
  toParams: (input) => ({
    tool: input.tool,
    name: input.name,
    ...(input.port === null ? {} : { port: input.port }),
    autostart: input.autostart,
    ...(input.dataDir === "" ? {} : { data_dir: input.dataDir }),
  }),
});

/** Removes an instance; the daemon refuses without `confirm: true` (destructive). */
export const removeInstance = defineAction<
  "services.remove",
  { instance: string; deleteData: boolean }
>({
  id: "services.remove_instance",
  method: "services.remove",
  toParams: ({ instance, deleteData }) => ({
    instance,
    delete_data: deleteData,
    confirm: true,
  }),
});

/** Ends the process that holds a port (local only; the user confirmed which one). */
export const killPortHolder = defineAction<
  "services.kill_port_holder",
  { port: number; pid: number }
>({
  id: "services.kill_port_holder",
  method: "services.kill_port_holder",
  toParams: ({ port, pid }) => ({ port, pid, confirm: true }),
});

/** Applies the changed quick settings (`forge service set php@8.4 memory_limit=1G`). */
export const setServiceSettings = defineAction<
  "services.set",
  { target: string; settings: Record<string, string> }
>({
  id: "services.set_settings",
  method: "services.set",
  toParams: ({ target, settings }) => ({ target, settings }),
});

/** Opens the config files of a target in the editor (local only). */
export const openServiceConfig = defineAction<"services.config", string>({
  id: "services.open_config",
  method: "services.config",
  toParams: (target) => ({ target }),
});

/** The advanced settings of an instance; absent fields keep their value. */
export type AdvancedPatch = Omit<ServiceUpdateParams, "instance">;

/** Updates the advanced settings of an instance (`forge service update mysql/default ...`). */
export const updateInstance = defineAction<
  "services.update",
  { instance: string; patch: AdvancedPatch }
>({
  id: "services.update_instance",
  method: "services.update",
  toParams: ({ instance, patch }) => ({ instance, ...patch }),
});

/** Reloads one service's configuration without a restart (`forge reload php@8.4`). */
export const reloadService = defineAction<"services.reload", string>({
  id: "services.reload",
  method: "services.reload",
  toParams: (target) => ({ target }),
});
