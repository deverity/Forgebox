// Tool-version actions of the Versions tab (docs/07 §5.3). Registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the parity gate see them.
import type { UpdatePolicy } from "../ipc/generated";
import { defineAction } from "./registry";

/** Points the CLI `current` at a version (`forge use php 8.4`); sites and services keep theirs. */
export const setCliVersion = defineAction<"tools.use", { tool: string; version: string }>({
  id: "tools.use_version",
  method: "tools.use",
  toParams: ({ tool, version }) => ({ tool, version }),
});

/** Removes one installed version (`forge tool remove php@8.3.36`); destructive, so `confirm`. */
export const removeToolVersion = defineAction<"tools.remove", string>({
  id: "tools.remove_version",
  method: "tools.remove",
  toParams: (spec) => ({ spec, confirm: true }),
});

/** Undoes the last update of a series (`forge tool rollback php@8.3`). */
export const rollbackToolSeries = defineAction<"tools.rollback", string>({
  id: "tools.rollback_series",
  method: "tools.rollback",
  toParams: (spec) => ({ spec }),
});

/** Looks for newer versions of one tool (`forge tool update php --check`). */
export const checkToolUpdates = defineAction<"tools.check_updates", string>({
  id: "tools.check_updates_one",
  method: "tools.check_updates",
  toParams: (tool) => ({ tool }),
});

/** Sets the update policy of a tool (`forge tool policy php notify`). */
export const setToolPolicy = defineAction<"tools.policy", { tool: string; policy: UpdatePolicy }>({
  id: "tools.set_policy",
  method: "tools.policy",
  toParams: ({ tool, policy }) => ({ tool, policy }),
});

/**
 * Installs tool versions (`forge tool install php@8.4.27 --variant nts`). `yes` is set because
 * the catalog shows the `tools.plan` (dependents and total size) before the user confirms.
 */
export const installTools = defineAction<
  "tools.install",
  { specs: readonly string[]; variant?: string | null }
>({
  id: "tools.install_specs",
  method: "tools.install",
  toParams: ({ specs, variant }) => ({
    specs: [...specs],
    yes: true,
    ...(variant === undefined || variant === null || variant === "" ? {} : { variant }),
  }),
});

/** Registers loose folders found in `bin/` as tool versions (`forge tool discover`). */
export const discoverTools = defineAction({
  id: "tools.discover",
  method: "tools.discover",
  toParams: () => ({}),
});

/** Applies the updates found for one tool (`forge tool update php`). */
export const updateTool = defineAction<"tools.update", string>({
  id: "tools.update",
  method: "tools.update",
  toParams: (tool) => ({ tool }),
});
