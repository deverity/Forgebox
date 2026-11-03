import { describe, expect, it } from "vitest";
import { ForgeApiError } from "../../../ipc/errors";
import {
  checkTone,
  currentSeries,
  isNotInstalled,
  phpOptions,
  phpSpec,
  planSize,
  processOfEvent,
  seriesOf,
} from "./model";

describe("php helpers", () => {
  it("builds specs and series", () => {
    expect(phpSpec("8.4")).toBe("php@8.4");
    expect(seriesOf("8.4.27")).toBe("8.4");
    expect(seriesOf("8.4")).toBe("8.4");
  });

  it("takes the pinned series, else the resolved one", () => {
    expect(currentSeries({ series: "8.3", exact: null, resolved: "8.3.36", upstream: null })).toBe(
      "8.3",
    );
    expect(currentSeries({ series: null, exact: "8.2.30", resolved: null, upstream: null })).toBe(
      "8.2",
    );
    expect(currentSeries(null)).toBeNull();
  });

  it("merges installed and available series, newest first, marking the missing ones", () => {
    expect(phpOptions(["8.3", "8.2"], ["8.5", "8.3"], "8.3")).toEqual([
      { value: "8.5", installed: false },
      { value: "8.3", installed: true },
      { value: "8.2", installed: true },
    ]);
  });
});

describe("isNotInstalled", () => {
  it("recognises the version_not_installed rejection of sites.php", () => {
    const details = { reason: "version_not_installed", args: { installed: ["8.3.36"] } };
    expect(isNotInstalled(new ForgeApiError({ code: "not_found", message: "x", details }))).toBe(
      true,
    );
    expect(isNotInstalled(new ForgeApiError({ code: "not_found", message: "x" }))).toBe(false);
    expect(isNotInstalled(new ForgeApiError({ code: "conflict", message: "x", details }))).toBe(
      false,
    );
  });
});

describe("planSize", () => {
  it("is approximate when a size is estimated or missing", () => {
    const item = { tool: "php", spec: "php@8.4", version: "8.4.0", reason: "requested" } as const;
    expect(planSize({ items: [{ ...item, size_estimated: true }], total_bytes: 10 })).toEqual({
      bytes: 10,
      approximate: true,
      nothing: false,
    });
    expect(
      planSize({ items: [{ ...item, already_installed: true }], total_bytes: 0 }).nothing,
    ).toBe(true);
  });
});

describe("processOfEvent", () => {
  const row = (name: string, instance: string | null) => ({
    site_id: "s",
    host: "h",
    name,
    command: "c",
    state: "stopped" as const,
    pid: null,
    port: null,
    autostart: false,
    instance_id: instance,
  });
  const payload = (instance: string, name: string) =>
    ({ v: 1, instance_id: instance, name, state: "running" }) as never;

  it("matches by instance id, else by process name", () => {
    const rows = [row("web", "I1"), row("queue", null)];
    expect(processOfEvent(rows, payload("I1", "x"))?.name).toBe("web");
    expect(processOfEvent(rows, payload("I9", "loja.test/queue"))?.name).toBe("queue");
    expect(processOfEvent(rows, payload("I9", "loja.test/other"))).toBeUndefined();
  });
});

describe("checkTone", () => {
  it("maps each status", () => {
    expect(["ok", "warn", "fail", "skipped"].map((s) => checkTone(s as never))).toEqual([
      "success",
      "warning",
      "danger",
      "neutral",
    ]);
  });
});
