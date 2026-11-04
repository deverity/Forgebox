// The `forge` commands equivalent to what the wizard does (docs/07 section 1.7), rendered by
// the same actions the wizard runs so the panel can never drift from the real calls.
import { enableDns, installCa, addToPath } from "../../actions/diagnostics";
import { parkDirectory } from "../../actions/onboarding";
import { setConfig } from "../../actions/system";
import { installTools } from "../../actions/tools";
import { DEFAULT_TLD, normalizeTld, type Choices, type RunStepId } from "./model";

export interface CommandRow {
  /** Step the command belongs to. */
  id: string;
  command: string;
}

/** The command of one run step (`null`: the step has none, e.g. the Windows startup entry). */
export function stepCommand(id: RunStepId, specs: readonly string[] = []): string | null {
  switch (id) {
    case "parity":
      return setConfig.cli({ key: "case_sensitive", value: false });
    case "install":
      return specs.length === 0 ? null : installTools.cli({ specs });
    case "path":
      return addToPath.cli();
    case "dns":
      return enableDns.cli();
    case "ca":
      return installCa.cli({ scope: "current_user" });
    case "autostart":
      return null;
  }
}

/** Rows of "Comandos equivalentes", in the order the wizard runs them. */
export function equivalentCommands(
  choices: Choices,
  options: { showRoot: boolean; steps: readonly RunStepId[]; specs: readonly string[] },
): CommandRow[] {
  const rows: CommandRow[] = [];
  const tld = normalizeTld(choices.tld);
  if (options.showRoot) {
    if (tld !== DEFAULT_TLD) {
      const command = setConfig.cli({ key: "tld", value: tld });
      if (command !== null) rows.push({ id: "tld", command });
    }
    const park = parkDirectory.cli({ dir: choices.parkedDir, tld });
    if (park !== null) rows.push({ id: "park", command: park });
  }
  for (const id of options.steps) {
    const command = stepCommand(id, options.specs);
    if (command !== null) rows.push({ id, command });
  }
  return rows;
}
