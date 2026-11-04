// Mutations of the onboarding wizard (docs/07 section 5.1). The PATH, DNS, CA and install
// steps reuse the registered actions of `diagnostics.ts` and `tools.ts`, and config writes use
// `setConfig` of `system.ts`; only the park action is wizard-specific.
import { defineAction } from "./registry";

/** `forge park <dir> --tld .test`: the folder whose subfolders become sites. */
export const parkDirectory = defineAction<"sites.park", { dir: string; tld: string }>({
  id: "onboarding.park",
  method: "sites.park",
  toParams: ({ dir, tld }) => ({ dir, tld }),
});
