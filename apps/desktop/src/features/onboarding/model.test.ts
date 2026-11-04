import { describe, expect, it } from "vitest";
import { ForgeApiError } from "../../ipc/errors";
import { equivalentCommands, stepCommand } from "./commands";
import {
  defaultChoices,
  defaultParkedDir,
  isElevationRefused,
  normalizeTld,
  parseChoices,
  planParams,
  runPlan,
  tldProblem,
  visibleSteps,
} from "./model";

describe("tldProblem", () => {
  it.each([
    [".test", null],
    ["TEST", null],
    ["dev.test", null],
    [".home.arpa", null],
    [".lan", null],
    ["", "empty"],
    [" . ", "empty"],
    [".my tld", "syntax"],
    [".-bad", "syntax"],
    [".a..b", "syntax"],
    [".com", "reserved"],
    [".dev", "reserved"],
    [".testing", "reserved"],
  ])("%s -> %s", (raw, expected) => {
    expect(tldProblem(raw)).toBe(expected);
  });

  it("normalizes case, spaces and the leading dot", () => {
    expect(normalizeTld(" ..Dev.Test ")).toBe(".dev.test");
    expect(normalizeTld("")).toBe("");
  });
});

describe("choices", () => {
  it("defaults to PATH, DNS, CA and parity on and the startup entry off", () => {
    expect(defaultChoices()).toMatchObject({
      root: "C:/Forgebox",
      parkedDir: "C:/Forgebox/www",
      tld: ".test",
      path: true,
      dns: true,
      ca: true,
      autostart: false,
      parity: true,
    });
  });

  it("derives the parked directory from the root until the user edits it", () => {
    expect(defaultParkedDir("D:\\Dev\\Forge\\")).toBe("D:/Dev/Forge/www");
    expect(parseChoices({ root: "D:/Dev" }, "C:/Forgebox").parkedDir).toBe("D:/Dev/www");
    expect(
      parseChoices({ root: "D:/Dev", parkedEdited: true, parkedDir: "E:/Sites" }, "C:/Forgebox")
        .parkedDir,
    ).toBe("E:/Sites");
  });

  it("ignores mistyped stored values", () => {
    const choices = parseChoices(
      { dns: "yes", stack: 4, versions: { php: "8.3", node: 9 }, applied: "x" },
      "C:/Forgebox",
    );
    expect(choices.dns).toBe(true);
    expect(choices.stack).toBeNull();
    expect(choices.versions).toEqual({ php: "8.3" });
    expect(choices.applied).toEqual({ root: "", tld: "", park: "" });
  });
});

describe("steps", () => {
  it("drops the root step when it does not apply", () => {
    expect(visibleSteps(true)).toEqual(["root", "stack", "options", "run"]);
    expect(visibleSteps(false)).toEqual(["stack", "options", "run"]);
  });

  it("orders the run as the acceptance criteria do", () => {
    const options = { pathAvailable: true, autostartAvailable: true };
    expect(runPlan(defaultChoices(), options)).toEqual(["install", "path", "dns", "ca"]);
    expect(runPlan({ ...defaultChoices(), parity: false, autostart: true }, options)).toEqual([
      "parity",
      "install",
      "path",
      "dns",
      "ca",
      "autostart",
    ]);
    expect(
      runPlan(
        { ...defaultChoices(), autostart: true },
        { pathAvailable: false, autostartAvailable: false },
      ),
    ).toEqual(["install", "dns", "ca"]);
  });

  it("plans a stack with the version overrides as specs", () => {
    expect(planParams("laravel", { php: "8.3" })).toEqual({
      stack: "laravel",
      specs: ["php@8.3"],
    });
  });

  it("recognizes a refused UAC by code or reason", () => {
    expect(isElevationRefused(new ForgeApiError({ code: "needs_elevation", message: "x" }))).toBe(
      true,
    );
    expect(
      isElevationRefused(
        new ForgeApiError({
          code: "internal",
          message: "x",
          details: { reason: "needs_elevation", args: {} },
        }),
      ),
    ).toBe(true);
    expect(isElevationRefused(new ForgeApiError({ code: "io", message: "x" }))).toBe(false);
  });
});

describe("commands", () => {
  it("renders the commands of the steps through the actions", () => {
    expect(stepCommand("dns")).toBe("forge dns enable");
    expect(stepCommand("path")).toBe("forge path add");
    expect(stepCommand("ca")).toBe("forge tls ca install --scope user");
    expect(stepCommand("parity")).toBe("forge config set case_sensitive false");
    expect(stepCommand("install", ["php@8.4", "mysql@8.4"])).toBe(
      "forge tool install php@8.4 mysql@8.4",
    );
    expect(stepCommand("install")).toBeNull();
    expect(stepCommand("autostart")).toBeNull();
  });

  it("lists the folder commands only when step 1 applied, and a TLD change before the park", () => {
    const choices = { ...defaultChoices("C:/Meus Sites"), tld: "dev.test" };
    const rows = equivalentCommands(choices, {
      showRoot: true,
      steps: ["install", "dns"],
      specs: ["php@8.4"],
    });
    expect(rows.map((row) => row.command)).toEqual([
      "forge config set tld .dev.test",
      'forge park "C:/Meus Sites/www" --tld .dev.test',
      "forge tool install php@8.4",
      "forge dns enable",
    ]);
    expect(
      equivalentCommands(choices, { showRoot: false, steps: ["dns"], specs: [] }).map(
        (row) => row.command,
      ),
    ).toEqual(["forge dns enable"]);
  });
});
