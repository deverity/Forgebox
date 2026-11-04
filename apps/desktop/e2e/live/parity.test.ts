// The parity scan of the live suite must itself fail on a bad page (M2-E2-T20): an action
// without a command, one the golden file does not know, a verb that is not the action's, and a
// line the real `forge` rejects are each reported.
import { describe, expect, it } from "vitest";
import { loadGolden, parityProblems, words, type ScannedAction } from "./parity";

const ok = () => null;
const item = (action: string, cli: string): ScannedAction => ({ route: "/x", action, cli });

describe("live parity scan", () => {
  it("splits a command line like a shell", () => {
    expect(words(String.raw`forge db export loja --output "D:\My Dir\a.sql"`)).toEqual([
      "forge",
      "db",
      "export",
      "loja",
      "--output",
      String.raw`D:\My Dir\a.sql`,
    ]);
  });

  it("accepts the commands of the golden file itself", () => {
    const golden = loadGolden();
    const scanned = [...golden.values()].flatMap((g) =>
      g.samples.flatMap((s) => (s.command === null ? [] : [item(g.action, s.command)])),
    );
    expect(scanned.length).toBeGreaterThan(50);
    expect(parityProblems(scanned, ok)).toEqual([]);
  });

  it("reports the four ways a page can break the contract", () => {
    const problems = parityProblems(
      [
        item("services.start_instance", ""),
        item("made.up_action", "forge made up"),
        item("services.start_instance", "forge frobnicate mysql/default"),
        item("services.start_instance", "forge start mysql/default --nope"),
      ],
      (cli) => (cli.includes("--nope") ? "unexpected argument '--nope'" : null),
    );
    expect(problems).toEqual([
      "/x: services.start_instance has no data-forge-cli",
      "/x: made.up_action is not in qa/parity/gui-commands.json",
      expect.stringContaining("does not start like the golden commands"),
      expect.stringContaining("forge rejects `forge start mysql/default --nope`"),
    ]);
  });
});
