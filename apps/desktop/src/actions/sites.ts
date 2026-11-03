// Site actions (docs/07 section 5.4). Registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the E4-T5 parity gate see every mutation of the Sites screen.
import type { ParamsOf } from "../ipc/api";
import { defineAction } from "./registry";

/** "Pasta" tab of the create sheet: `forge site add`. */
export const addSite = defineAction<"sites.add", ParamsOf<"sites.add">>({
  id: "sites.add",
  method: "sites.add",
  toParams: (input) => input,
});

/** "Proxy" tab of the create sheet: `forge proxy add`. */
export const addProxy = defineAction<"sites.proxy_add", ParamsOf<"sites.proxy_add">>({
  id: "sites.proxy_add",
  method: "sites.proxy_add",
  toParams: (input) => input,
});

/** Removes a site; the project files are never deleted, the daemon requires `confirm: true`. */
export const removeSite = defineAction<"sites.remove", string>({
  id: "sites.remove",
  method: "sites.remove",
  toParams: (site) => ({ site, confirm: true }),
});
