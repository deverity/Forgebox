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

/** Live PHP switch of one site (`forge site php loja.test 8.4`, docs/06 section 2.2). */
export const switchSitePhp = defineAction<"sites.php", { site: string; php: string }>({
  id: "sites.switch_php",
  method: "sites.php",
  toParams: ({ site, php }) => ({ site, php }),
});

/** Opens the site on the daemon's machine (`local_only`): browser, explorer, editor, terminal. */
export const openSite = defineAction<
  "sites.open",
  { site: string; with: ParamsOf<"sites.open">["with"] }
>({
  id: "sites.open",
  method: "sites.open",
  toParams: ({ site, with: target }) => ({ site, with: target }),
});

/** Start, stop or restart one Procfile process (`forge run loja.test web start`). */
export const runSiteProcess = defineAction<
  "sites.run",
  { site: string; process: string; action: ParamsOf<"sites.run">["action"] }
>({
  id: "sites.run_process",
  method: "sites.run",
  toParams: ({ site, process, action }) => ({ site, process, action }),
});

/** Overrides the framework driver; `driver: null` detects it again (`forge site driver X auto`). */
export const setSiteDriver = defineAction<"sites.driver", { site: string; driver: string | null }>({
  id: "sites.set_driver",
  method: "sites.driver",
  toParams: ({ site, driver }) => ({ site, driver }),
});

/** Installs a missing tool version after the user confirmed (`forge tool install php@8.4`). */
export const installToolSpec = defineAction<"tools.install", string>({
  id: "tools.install_spec",
  method: "tools.install",
  toParams: (spec) => ({ specs: [spec] }),
});

/** Starts the project's services and processes from its `forgebox.yaml` (`forge up <dir>`). */
export const projectUp = defineAction<"sites.up", string>({
  id: "sites.up",
  method: "sites.up",
  toParams: (dir) => ({ dir }),
});

/** Stops what `forge up` started for the project folder (`forge down <dir>`). */
export const projectDown = defineAction<"sites.down", string>({
  id: "sites.down",
  method: "sites.down",
  toParams: (dir) => ({ dir }),
});

/** Turns HTTPS on or off for a site (`forge site secure|unsecure loja.test`). */
export const setSiteSecure = defineAction<"sites.secure", { site: string; secure: boolean }>({
  id: "sites.secure",
  method: "sites.secure",
  toParams: ({ site, secure }) => ({ site, secure }),
});

/** Moves a site to another web server tool (`forge site server loja.test nginx`). */
export const setSiteServer = defineAction<"sites.server", { site: string; server: string }>({
  id: "sites.server",
  method: "sites.server",
  toParams: ({ site, server }) => ({ site, server }),
});
