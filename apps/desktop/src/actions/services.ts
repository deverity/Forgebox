// Service actions (docs/07 §1.7). Mutating methods are registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the E4-T5 parity gate can see every one of them.
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
