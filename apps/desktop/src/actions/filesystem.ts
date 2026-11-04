// Actions of Settings > Sistema de arquivos, Registries and Atualizacoes (docs/07 section 5.11,
// M2-E3-T18). The long ones are jobs; the screen follows them through `useJobAction`.
import type { CaseFixChoice } from "../ipc/generated";
import { defineAction } from "./registry";

/** `forge fs case-sensitive enable <dir>`: flags an (empty) folder case-sensitive. */
export const enableCaseDir = defineAction<"fs_parity.case_enable", string>({
  id: "filesystem.case_enable",
  method: "fs_parity.case_enable",
  toParams: (dir) => ({ dir }),
});

/** `forge fs case-sensitive disable <dir>`. */
export const disableCaseDir = defineAction<"fs_parity.case_disable", string>({
  id: "filesystem.case_disable",
  method: "fs_parity.case_disable",
  toParams: (dir) => ({ dir }),
});

/** `forge fs case-sensitive convert <dir> [--revert]`: moves a tree into a flagged folder. */
export const convertCaseDir = defineAction<
  "fs_parity.case_convert",
  { dir: string; revert: boolean }
>({
  id: "filesystem.case_convert",
  method: "fs_parity.case_convert",
  toParams: ({ dir, revert }) => ({ dir, ...(revert ? { revert: true } : {}) }),
});

/** Input of the Case Mismatch Finder. */
export interface LintCaseInput {
  site: string;
  strategy: CaseFixChoice;
  /** Dry run: plan and preview only; otherwise the fixes are applied. */
  dryRun: boolean;
}

/** `forge site lint-case <site> --fix --dry-run|` (the apply form is only sent after confirm). */
export const lintSiteCase = defineAction<"fs_parity.lint_case", LintCaseInput>({
  id: "filesystem.lint_case",
  method: "fs_parity.lint_case",
  toParams: ({ site, strategy, dryRun }) => ({
    site,
    fix: true,
    ...(dryRun ? { dry_run: true } : {}),
    strategy,
    only: null,
  }),
});

/** `forge registry add <url> [--name <name>]`. */
export const addRegistry = defineAction<"registry.add", { url: string; name?: string }>({
  id: "registries.add",
  method: "registry.add",
  toParams: ({ url, name }) => ({ url, ...(name === undefined || name === "" ? {} : { name }) }),
});

/** `forge registry remove <name>`. */
export const removeRegistry = defineAction<"registry.remove", string>({
  id: "registries.remove",
  method: "registry.remove",
  toParams: (name) => ({ name }),
});

/** `forge registry update`: refreshes every enabled registry (job). */
export const updateRegistries = defineAction({
  id: "registries.update",
  method: "registry.update",
  toParams: () => ({}),
});

/** `forge tool update --check`: "Verificar agora" for every tool (job). */
export const checkAllUpdates = defineAction({
  id: "updates.check_all",
  method: "tools.check_updates",
  toParams: () => ({}),
});
