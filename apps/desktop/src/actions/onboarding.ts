// Mutations of the onboarding wizard (docs/07 section 5.1). The PATH, DNS, CA and install
// steps reuse the registered actions of `diagnostics.ts` and `tools.ts`, and config writes use
// `setConfig` of `system.ts`; only the park action is wizard-specific.
import { defineAction } from "./registry";

/**
 * `forge fs case-sensitive disable <dir>`: step 3 with Linux parity off. The root step already
 * created the parked folder with the case-sensitive flag, and `case_sensitive` is read only
 * when a folder is created (docs/15 section 3.1), so the flag is cleared while it is empty.
 */
export const disableCaseSensitivity = defineAction<"fs_parity.case_disable", { dir: string }>({
  id: "onboarding.case_disable",
  method: "fs_parity.case_disable",
  toParams: ({ dir }) => ({ dir }),
});

/** `forge park <dir> --tld .test`: the folder whose subfolders become sites. */
export const parkDirectory = defineAction<"sites.park", { dir: string; tld: string }>({
  id: "onboarding.park",
  method: "sites.park",
  toParams: ({ dir, tld }) => ({ dir, tld }),
});
