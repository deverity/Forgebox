import { describe, expect, it } from "vitest";
import type { CaseIssueDto, CaseSensitivityDto } from "../../../../ipc/generated";
import { caseState, fixableCount, isRevertConflict, issuePath, joinPath } from "./model";

const status = (patch: Partial<CaseSensitivityDto>): CaseSensitivityDto => ({
  dir: "C:/fb/www",
  supported: true,
  enabled: true,
  fs_type: "NTFS",
  os_build: 19045,
  win_version_ok: true,
  reason: null,
  ...patch,
});

describe("filesystem model", () => {
  it("joins with forward slashes and no double separator", () => {
    expect(joinPath("C:\\forgebox\\", "www")).toBe("C:/forgebox/www");
    expect(joinPath("C:/forgebox", "etc")).toBe("C:/forgebox/etc");
  });

  it("classifies a folder", () => {
    expect(caseState(status({}))).toBe("enabled");
    expect(caseState(status({ enabled: false }))).toBe("disabled");
    expect(caseState(status({ supported: false, enabled: null }))).toBe("unsupported");
  });

  it("builds the editor path of a finding", () => {
    expect(issuePath("C:/fb/www/loja", "app/Views/header.php")).toBe(
      "C:/fb/www/loja/app/Views/header.php",
    );
    expect(issuePath("C:/fb/www/loja/", "/a.php")).toBe("C:/fb/www/loja/a.php");
  });

  it("counts only fixable findings", () => {
    const issue = (fixes: CaseIssueDto["fixes"]) => ({ fixes }) as CaseIssueDto;
    expect(fixableCount([issue(["rename"]), issue([]), issue(["rename", "edit"])])).toBe(2);
  });

  it("recognises a revert conflict only on revert", () => {
    const collisions = { code: "conflict", reason: "case_collisions" };
    expect(isRevertConflict(collisions, true)).toBe(true);
    expect(isRevertConflict(collisions, false)).toBe(false);
    expect(isRevertConflict({ code: "conflict" }, true)).toBe(false);
    expect(isRevertConflict({ code: "io", reason: "case_collisions" }, true)).toBe(false);
    expect(isRevertConflict(null, true)).toBe(false);
  });
});
