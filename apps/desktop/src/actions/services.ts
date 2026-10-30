// Service actions (docs/07 §1.7). Mutating methods are registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the E4-T5 parity gate can see every one of them.
import { defineAction } from "./registry";

/** "Start all": `services.start` with no targets (the daemon reads that as all). */
export const startAllServices = defineAction({
  id: "services.start",
  method: "services.start",
  toParams: () => ({}),
});
