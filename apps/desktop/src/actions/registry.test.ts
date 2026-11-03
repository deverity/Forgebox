import { afterEach, describe, expect, it } from "vitest";
import { defineAction, getAction, listActions, resetActionsForTest } from "./registry";

afterEach(resetActionsForTest);

describe("action registry", () => {
  it("defines an action whose cli comes from the same params as the call", () => {
    const start = defineAction({
      id: "services.start",
      method: "services.start",
      toParams: (targets: string[]) => ({ targets }),
    });
    expect(start.kind).toBe("job");
    expect(start.cli(["mysql", "redis"])).toBe("forge start mysql redis");
    expect(getAction("services.start")).toBe(start);
    expect(listActions()).toHaveLength(1);
  });

  it("refuses duplicate ids and streams", () => {
    defineAction({ id: "dns.enable", method: "net.dns_enable", toParams: () => ({}) });
    expect(() =>
      defineAction({ id: "dns.enable", method: "net.dns_enable", toParams: () => ({}) }),
    ).toThrow(/already defined/);
    expect(() =>
      defineAction({
        id: "events",
        method: "events.subscribe",
        toParams: () => ({ filter: { kinds: [] }, resume_from: null }),
      }),
    ).toThrow(/stream/);
  });
});
