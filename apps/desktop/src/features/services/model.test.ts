import { describe, expect, it } from "vitest";
import { instance, pool, tool } from "../../state/testFixtures";
import { dotState, groupRows, mainPort, portHolder, targetOf, uptimeMs } from "./model";

describe("groupRows", () => {
  const tools = {
    mysql: tool("mysql", { category: "database" }),
    nginx: tool("nginx", { category: "webserver" }),
    php: tool("php", { category: "runtime" }),
  };

  it("orders known categories first, then unknown ones, then other", () => {
    const groups = groupRows(
      [
        instance("1", "zzz/default"),
        instance("2", "mysql/b"),
        instance("3", "nginx/default"),
        instance("4", "mysql/a"),
      ],
      [pool("V1")],
      { ...tools, zzz: tool("zzz", { category: "aaa-new" }) },
    );
    expect(groups.map((g) => g.category)).toEqual(["webserver", "database", "runtime", "aaa-new"]);
    expect(groups[1]?.rows.map((r) => r.key)).toEqual(["4", "2"]);
  });

  it("puts tools without a category under other", () => {
    const groups = groupRows([instance("1", "mystery/default")], [], tools);
    expect(groups).toHaveLength(1);
    expect(groups[0]?.category).toBe("other");
  });
});

describe("helpers", () => {
  it("maps daemon states to the five dots", () => {
    expect(dotState("stopping")).toBe("starting");
    expect(dotState("crashed")).toBe("failed");
    expect(dotState("maintenance")).toBe("updating");
    expect(dotState("running")).toBe("running");
  });

  it("builds targets, main ports and uptime", () => {
    const inst = instance("1", "mysql/default");
    expect(targetOf(inst)).toBe("mysql/default");
    expect(mainPort(inst)).toBeNull();
    expect(uptimeMs(null, 10)).toBeNull();
    expect(uptimeMs("2026-01-01T00:00:00Z", Date.parse("2026-01-01T00:00:05Z"))).toBe(5000);
  });

  it("reads the port holder from the diagnosis, flat or under args", () => {
    expect(portHolder({ details: { port: 80, pid: 5, exe: "a.exe" } })).toEqual({
      port: 80,
      pid: 5,
      exe: "a.exe",
    });
    expect(portHolder({ details: { args: { port: 80, pid: 5 } } })).toEqual({
      port: 80,
      pid: 5,
      exe: null,
    });
    expect(portHolder({ details: { port: 80 } })).toBeNull();
    expect(portHolder(null)).toBeNull();
  });
});
