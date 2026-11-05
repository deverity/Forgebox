// The parity scan of the E2 live suite (M2-E2-T20, docs/07 section 1.7, ADR-0037): every
// `[data-action]` on the E2 routes carries a non-empty `data-forge-cli`, its action id is in the
// golden file `qa/parity/gui-commands.json` (whose commands the Rust half, crates/forge-cli/tests/
// gui_parity.rs, parses with the real clap tree and maps back to the method), the verb of the
// rendered command is one the golden file has for that action, and the real `forge` binary
// accepts the rendered command line (`forge <args> --help` exits 0: clap resolved every
// subcommand, flag and positional of this very line).
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";

const here = dirname(fileURLToPath(import.meta.url));

export interface ScannedAction {
  route: string;
  action: string;
  cli: string;
}

export interface Golden {
  action: string;
  method: string;
  samples: { command: string | null }[];
}

/** Every `[data-action]` of the page now, with the command the element shows. */
export async function scanActions(page: Page, route: string): Promise<ScannedAction[]> {
  return page.evaluate(
    (r) =>
      [...document.querySelectorAll("[data-action]")].map((el) => ({
        route: r,
        action: el.getAttribute("data-action") ?? "",
        cli: (el.getAttribute("data-forge-cli") ?? "").trim(),
      })),
    route,
  );
}

/** Splits a command line the way a shell would (double quotes group a word). */
export function words(line: string): string[] {
  const out: string[] = [];
  const re = /"((?:[^"\\]|\\.)*)"|(\S+)/g;
  for (let m = re.exec(line); m !== null; m = re.exec(line)) out.push(m[1] ?? m[2] ?? "");
  return out;
}

export function loadGolden(): Map<string, Golden> {
  const file = join(here, "../../qa/parity/gui-commands.json");
  const list = JSON.parse(readFileSync(file, "utf8")) as Golden[];
  return new Map(list.map((g) => [g.action, g]));
}

/** `null` when the real `forge` accepts the line, else the first line of its complaint. */
export type ForgeCheck = (cli: string) => string | null;

export function forgeAccepts(forgeExe: string): ForgeCheck {
  return (cli) => {
    const run = spawnSync(forgeExe, [...words(cli).slice(1), "--help"], { encoding: "utf8" });
    return run.status === 0 ? null : (run.stderr.trim().split("\n")[0] ?? "rejected");
  };
}

/** The problems of the scanned actions; an empty list is a pass. */
export function parityProblems(
  scanned: ScannedAction[],
  check: ForgeCheck,
  golden: Map<string, Golden> = loadGolden(),
): string[] {
  const problems: string[] = [];
  const checked = new Map<string, string | null>();
  for (const item of scanned) {
    const where = `${item.route}: ${item.action}`;
    if (item.cli === "") {
      problems.push(`${where} has no data-forge-cli`);
      continue;
    }
    const entry = golden.get(item.action);
    if (!entry) {
      problems.push(`${where} is not in qa/parity/gui-commands.json`);
      continue;
    }
    const verbs = new Set(
      entry.samples.flatMap((s) => (s.command === null ? [] : [words(s.command)[1] ?? ""])),
    );
    const verb = words(item.cli)[1] ?? "";
    if (words(item.cli)[0] !== "forge" || !verbs.has(verb)) {
      problems.push(`${where}: \`${item.cli}\` does not start like the golden commands (${[...verbs].join(", ")})`);
      continue;
    }
    if (!checked.has(item.cli)) {
      checked.set(item.cli, check(item.cli));
    }
    const bad = checked.get(item.cli);
    if (bad !== null && bad !== undefined) problems.push(`${where}: forge rejects \`${item.cli}\`: ${bad}`);
  }
  return problems;
}
