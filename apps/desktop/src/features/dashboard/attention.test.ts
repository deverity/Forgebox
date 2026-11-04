import { describe, expect, it } from "vitest";
import type * as T from "../../ipc/generated";
import {
  caItems,
  certItems,
  dnsItems,
  driftItems,
  driftedFiles,
  failedItems,
  holderName,
  pendingUpdates,
  portItems,
  updateItems,
} from "./attention";
import { recentSites, tilesOf } from "./model";
import { instance, pool, site, tool } from "../../state/testFixtures";

const NOW = Date.parse("2026-11-03T12:00:00Z");
const DAY = 24 * 60 * 60 * 1000;
const iso = (offsetMs: number) => new Date(NOW + offsetMs).toISOString();

function cert(host: string, notAfter: string): T.CertificateDto {
  return {
    host,
    sans: [host],
    site_id: null,
    not_before: iso(-DAY),
    not_after: notAfter,
    thumbprint_sha256: "AA",
  };
}

const CA: T.CaStatusDto = {
  exists: true,
  subject: "Forgebox Local CA",
  thumbprint_sha256: "AB",
  not_after: iso(3000 * DAY),
  trusted_current_user: true,
  trusted_local_machine: false,
  name_constraints: [],
};

describe("ports", () => {
  const conflict = {
    v: 1,
    port: 3306,
    protocol: "tcp" as const,
    owner_name: "mysql/default",
    instance_id: null,
    holder: { pid: 4, exe: "mysqld.exe" },
    ts: iso(0),
  };
  it("is one item per port, with the command", () => {
    const items = portItems({ "tcp/3306": conflict }, [
      { port: 3306, protocol: "tcp", holder: { pid: 4, exe: "mysqld.exe" } },
    ]);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ severity: "critical", cli: "forge service ports" });
    expect(portItems({}, [])).toEqual([]);
  });
  it("lists an external holder alone as a warning", () => {
    const items = portItems({}, [
      { port: 80, protocol: "tcp", holder: { pid: 9, exe: "httpd.exe" } },
    ]);
    expect(items).toHaveLength(1);
    expect(items[0]?.severity).toBe("warning");
    expect(items[0]?.title.values).toMatchObject({ port: 80, holder: "httpd.exe" });
  });
});

describe("updates", () => {
  it("is a single item for all pending updates", () => {
    const update = { tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" };
    expect(updateItems([update, { ...update, tool: "node" }])).toHaveLength(1);
    expect(updateItems([update])[0]?.cli).toBe("forge tool update --check");
    expect(updateItems([])).toEqual([]);
  });
});

describe("pending updates", () => {
  it("adds the tools whose list row has a newer version, once per tool", () => {
    const event = { tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" };
    const merged = pendingUpdates(
      [event],
      [
        tool("php", { current: "8.3.36", update_available: "8.3.37" }),
        tool("node", { current: "22.1.0", update_available: "22.2.0" }),
        tool("redis", { current: "7.0" }),
      ],
    );
    expect(merged.map((update) => update.tool)).toEqual(["php", "node"]);
    expect(updateItems(merged)).toHaveLength(1);
  });
});

describe("config drift (24 h)", () => {
  const drift = (path: string, ts: string) => ({
    kind: "config.drift_detected",
    ts,
    payload: { path },
  });
  it("keeps drifts of the last 24 h and drops older ones", () => {
    expect(driftedFiles([drift("a", iso(-23 * 60 * 60 * 1000))], {}, NOW)).toHaveLength(1);
    expect(driftedFiles([drift("a", iso(-25 * 60 * 60 * 1000))], {}, NOW)).toHaveLength(0);
  });
  it("a later config.rendered clears the file", () => {
    const history = [
      drift("a", iso(-3600_000)),
      { kind: "config.rendered", ts: iso(-1800_000), payload: { path: "a" } },
    ];
    expect(driftedFiles(history, {}, NOW)).toEqual([]);
  });
  it("adds what the stores saw live and builds one item per file", () => {
    const files = driftedFiles([], { b: { ts: iso(-1000) } }, NOW);
    const items = driftItems(files);
    expect(items).toHaveLength(1);
    expect(items[0]?.cli).toBe("forge doctor");
  });
});

describe("certificates", () => {
  it("flags the local CA when it is not trusted", () => {
    const items = caItems({ ...CA, trusted_current_user: false }, NOW);
    expect(items).toHaveLength(1);
    expect(items[0]?.cli).toMatch(/^forge tls ca install/);
  });
  it("flags the local CA at less than 60 days, not at 61", () => {
    expect(caItems({ ...CA, not_after: iso(59 * DAY + 1000) }, NOW)).toHaveLength(1);
    expect(caItems({ ...CA, not_after: iso(61 * DAY) }, NOW)).toHaveLength(0);
    expect(caItems({ ...CA, not_after: iso(10 * DAY) }, NOW)[0]?.cli).toBe("forge tls ca rotate");
  });
  it("stays quiet without a CA", () => {
    expect(caItems(null, NOW)).toEqual([]);
    expect(caItems({ ...CA, exists: false }, NOW)).toEqual([]);
  });
  it("flags site certificates at less than 30 days (expired too) as one item", () => {
    expect(certItems([cert("a.test", iso(29 * DAY))], NOW)).toHaveLength(1);
    expect(certItems([cert("a.test", iso(31 * DAY))], NOW)).toHaveLength(0);
    const both = certItems([cert("a.test", iso(5 * DAY)), cert("b.test", iso(-DAY))], NOW);
    expect(both).toHaveLength(1);
    expect(both[0]?.title.values).toMatchObject({ count: 2 });
    expect(both[0]?.cli).toBe("forge tls renew --all");
  });
});

describe("DNS", () => {
  const base: T.DnsStatusDto = {
    enabled: true,
    server_running: true,
    listen: "127.0.0.1:53",
    tlds: ["test"],
    policy: "applied",
    mode: "nrpt",
  };
  it("is quiet when NRPT routes it, or when the user left it off", () => {
    expect(dnsItems(base)).toEqual([]);
    expect(dnsItems({ ...base, enabled: false, policy: "not_applied", mode: "none" })).toEqual([]);
    expect(dnsItems(null)).toEqual([]);
  });
  it("explains why it is not routed and the hosts fallback", () => {
    const blocked = dnsItems({ ...base, mode: "none", reason: "group_policy_table_present" });
    expect(blocked).toHaveLength(1);
    expect(blocked[0]?.cause?.key).toBe("attention.dns.cause.group_policy_table_present");
    expect(blocked[0]?.cli).toBe("forge dns status");
    const hosts = dnsItems({ ...base, policy: "hosts_fallback", mode: "hosts" });
    expect(hosts[0]?.title.key).toBe("attention.dns.titleHosts");
    // A 1.0 daemon sends no `mode`: it is read from `policy`.
    const legacy: T.DnsStatusDto = { ...base, policy: "hosts_fallback" };
    delete legacy.mode;
    expect(dnsItems(legacy)).toHaveLength(1);
  });
});

describe("failed services", () => {
  it("is one item per failed or crashed instance", () => {
    const items = failedItems([
      instance("A", "redis/default", { state: "failed" }),
      instance("B", "mysql/default", { state: "running" }),
      instance("C", "mailpit/default", { state: "crashed" }),
    ]);
    expect(items.map((item) => item.cli)).toEqual([
      "forge restart redis/default",
      "forge restart mailpit/default",
    ]);
    expect(items[0]?.search).toEqual({ instance: "A" });
  });
});

describe("tiles and recent sites", () => {
  it("lists instances by name, then pools", () => {
    const tiles = tilesOf(
      [instance("B", "redis/default"), instance("A", "mysql/default")],
      [pool("P")],
    );
    expect(tiles.map((tile) => tile.label)).toEqual(["mysql/default", "redis/default", "php@8.3"]);
  });
  it("takes the 6 most recently updated sites", () => {
    const sites = Array.from({ length: 8 }, (_, i) =>
      site(`S${String(i)}`, `s${String(i)}.test`, { updated_at: iso(-i * 1000) }),
    );
    const recent = recentSites([...sites].reverse());
    expect(recent.map((s) => s.host)).toEqual([
      "s0.test",
      "s1.test",
      "s2.test",
      "s3.test",
      "s4.test",
      "s5.test",
    ]);
  });
});

describe("holderName (R-18)", () => {
  it("keeps the program name and drops the path", () => {
    expect(holderName({ pid: 1, exe: "mysqld.exe (C:/xampp/mysql/bin)" })).toBe("mysqld.exe");
    expect(holderName({ pid: 1, exe: "C:\\xampp\\mysql\\bin\\mysqld.exe" })).toBe("mysqld.exe");
  });
  it("treats an unknown holder as none", () => {
    expect(holderName(null)).toBeNull();
    expect(holderName({ pid: 0, exe: "?" })).toBeNull();
    expect(holderName({ pid: 0, exe: "  " })).toBeNull();
  });
  it("says another program instead of by ? in the title", () => {
    const [item] = portItems({}, [{ port: 3389, protocol: "tcp", holder: { pid: 4, exe: "?" } }]);
    expect(item?.title.key).toBe("attention.ports.titleUnknown");
  });
});
