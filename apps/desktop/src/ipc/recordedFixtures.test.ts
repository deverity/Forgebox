// The fakeBridge's not_implemented table must equal the one recorded from the real daemon
// (`cargo xtask qa-fixtures --only not-implemented`, M2-E4-T1): a stale copy would make the
// GUI tests disagree with forgeboxd.
import { describe, expect, it } from "vitest";

import recorded from "../../qa/fixtures/recorded/not_implemented.json";
import { NOT_IMPLEMENTED } from "./fakeBridge";

describe("recorded fixtures", () => {
  it("the fakeBridge not_implemented list equals the recorded one", () => {
    expect(NOT_IMPLEMENTED).toEqual(recorded);
  });

  it("recorded rows carry method, task and what", () => {
    for (const row of recorded) {
      expect(row.method).toMatch(/^[a-z_]+\.[a-z_]+$/);
      expect(row.task).toMatch(/^M\d+-E\d+$/);
      expect(row.what.length).toBeGreaterThan(0);
    }
  });
});
