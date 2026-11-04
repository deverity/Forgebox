import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type * as T from "../../ipc/generated";
import {
  buildBundle,
  bundleFileName,
  bundleJson,
  canFix,
  groupChecks,
  groupOf,
  mergeRerun,
  portOf,
  totals,
  type SupportBundle,
} from "./model";

function check(id: string, status: T.CheckStatus, patch: Partial<T.CheckDto> = {}): T.CheckDto {
  return { id, status, message: id, fix: null, fix_method: null, fix_params: null, ...patch };
}

const golden = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "support-bundle.golden.json"),
  "utf8",
);

describe("groups", () => {
  it("groups by the first id segment, in the documented order, with the worst status", () => {
    const groups = groupChecks([
      check("service.nginx", "fail"),
      check("port.80", "warn"),
      check("port.443", "ok"),
      check("zzz.custom", "ok"),
      check("dns.policy.test", "ok"),
    ]);
    expect(groups.map((g) => g.group)).toEqual(["dns", "port", "service", "zzz"]);
    expect(groups.find((g) => g.group === "port")?.worst).toBe("warn");
    expect(groups.find((g) => g.group === "port")?.checks).toHaveLength(2);
    expect(groupOf("path.user")).toBe("path");
    expect(groupOf("plain")).toBe("plain");
  });

  it("counts statuses and reads the port of port.N ids only", () => {
    expect(totals([check("a", "ok"), check("b", "warn"), check("c", "skipped")])).toEqual({
      ok: 1,
      warn: 1,
      fail: 0,
      skipped: 1,
    });
    expect(portOf(check("port.80", "warn"))).toBe(80);
    expect(portOf(check("port.x", "warn"))).toBeNull();
    expect(portOf(check("dns.server", "ok"))).toBeNull();
  });
});

describe("fix allowlist", () => {
  it("allows the three client fixes and nothing else", () => {
    for (const method of ["net.dns_enable", "tls.ca_install", "shell.path_add"]) {
      expect(canFix(check("x.y", "warn", { fix_method: method }))).toBe(true);
    }
    for (const method of ["daemon.stop", "fs.write", "toString", "__proto__", ""]) {
      expect(canFix(check("x.y", "warn", { fix_method: method }))).toBe(false);
    }
    expect(canFix(check("x.y", "warn"))).toBe(false);
    // Nothing to fix when the check is fine.
    expect(canFix(check("x.y", "ok", { fix_method: "net.dns_enable" }))).toBe(false);
  });
});

describe("mergeRerun", () => {
  it("replaces the rows of the group in place and drops the ones that disappeared", () => {
    const current = [
      check("root.writable", "ok"),
      check("dns.server", "ok"),
      check("dns.policy.test", "warn"),
      check("port.80", "warn"),
    ];
    const merged = mergeRerun(
      current,
      [check("dns.server", "ok"), check("dns.policy.test", "ok")],
      "dns",
    );
    expect(merged.map((c) => `${c.id}:${c.status}`)).toEqual([
      "root.writable:ok",
      "dns.server:ok",
      "dns.policy.test:ok",
      "port.80:warn",
    ]);
    expect(mergeRerun(current, [], "dns").map((c) => c.id)).toEqual(["root.writable", "port.80"]);
  });
});

describe("support bundle", () => {
  const fixture = JSON.parse(golden) as SupportBundle;
  const input = () => ({
    checks: fixture.doctor.checks,
    status: fixture.status,
    tools: [
      { id: "php", installed: [{ version: "8.4.27" }, { version: "8.3.36" }] },
      { id: "nginx", installed: [{ version: "1.28.0" }] },
    ] as unknown as T.ToolDto[],
    log: [{ text: "forgeboxd started" }, { text: "port 80 busy" }] as T.LogLineDto[],
    version: "0.2.0",
    apiVersion: { major: 1, minor: 1 },
    now: fixture.created_unix,
  });

  it("is byte for byte the shape the CLI writes (golden shared with a cargo test)", () => {
    expect(bundleJson(buildBundle(input()))).toBe(JSON.stringify(fixture, null, 2));
    expect(Object.keys(buildBundle(input()))).toEqual([
      "v",
      "created_unix",
      "forge_version",
      "api_version",
      "doctor",
      "status",
      "tools",
      "daemon_log",
    ]);
  });

  it("exports the rest when the daemon log cannot be read, like the CLI", () => {
    const bundle = buildBundle({ ...input(), log: null, logError: "not_implemented" });
    expect(bundle.daemon_log).toEqual(["(daemon log unavailable: not_implemented)"]);
    expect(bundle.tools).toHaveLength(2);
  });

  it("names the file by UTC time", () => {
    expect(bundleFileName(1793800000)).toBe("forgebox-diagnostics-20261104-134640.json");
  });
});
