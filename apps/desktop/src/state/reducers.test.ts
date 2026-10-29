import { describe, expect, it, vi } from "vitest";
import type * as T from "../ipc/generated";
import { EVENT_KINDS, METHODS } from "../ipc/methods";
import { EVENT_INVALIDATIONS } from "./queries";
import {
  EVENT_REDUCERS,
  commitTx,
  isIgnored,
  openTx,
  reduceEvents,
  type Ignored,
} from "./reducers";
import { createContextStores, type SliceName } from "./stores";
import { IDS, instance, pool, site, tool } from "./testFixtures";

let seq = 0;
function event(kind: string, payload: unknown, ts = "2026-10-28T12:00:00Z"): T.EventDto {
  seq += 1;
  return { id: `01JEVENT${String(seq).padStart(18, "0")}`, seq, ts, kind, payload };
}

function setup() {
  const stores = createContextStores("local");
  stores.services.setState({
    instances: {
      [IDS.mysql]: instance(IDS.mysql, "mysql/default"),
      [IDS.redis]: instance(IDS.redis, "redis/default"),
    },
    pools: { [IDS.php83]: pool(IDS.php83) },
    loaded: true,
  });
  stores.sites.setState({ sites: { [IDS.site]: site(IDS.site, "loja.test") }, loaded: true });
  stores.tools.setState({
    tools: {
      php: tool("php", {
        current: "8.3.36",
        installed: [
          {
            id: "01JVERS0000000000000PHP836",
            tool: "php",
            version: "8.3.36",
            canonical_name: "8.3.36",
            variant: {},
            state: "installed",
            source: "registry",
            path: "bin/php/8.3.36",
            size_bytes: null,
            installed_at: null,
            is_current: true,
          },
          {
            id: "01JVERS0000000000000PHP840",
            tool: "php",
            version: "8.4.0",
            canonical_name: "8.4.0",
            variant: {},
            state: "installed",
            source: "registry",
            path: "bin/php/8.4.0",
            size_bytes: null,
            installed_at: null,
            is_current: false,
          },
        ],
      }),
    },
  });
  const refreshed: SliceName[] = [];
  const invalidated: string[] = [];
  const resyncs: string[] = [];
  const apply = (events: T.EventDto[]) => {
    const tx = openTx(stores, {
      refresh: (slice) => refreshed.push(slice),
      invalidate: (methods) => invalidated.push(...methods),
      resync: (reason) => resyncs.push(reason),
    });
    reduceEvents(tx, events);
    return commitTx(tx);
  };
  return { stores, apply, refreshed, invalidated, resyncs };
}

function service(kind: string, id: string, patch: Partial<T.ServicePayload> = {}): T.EventDto {
  const state = kind.split(".")[1] as T.ServiceState;
  return event(kind, {
    v: 1,
    instance_id: id,
    name: "mysql/default",
    tool: "mysql",
    state,
    pid: state === "running" ? 4412 : null,
    strategy: null,
    exit_code: null,
    error: null,
    ...patch,
  } satisfies T.ServicePayload);
}

describe("reducer registry", () => {
  it("has a reducer or an explicit ignore for every EVENT_KINDS entry", () => {
    const registered = Object.keys(EVENT_REDUCERS).sort();
    const missing = EVENT_KINDS.filter((kind) => !(kind in EVENT_REDUCERS));
    expect(missing).toEqual([]);
    expect(registered).toEqual([...EVENT_KINDS].sort());
    for (const kind of EVENT_KINDS) {
      const entry = EVENT_REDUCERS[kind] as unknown as Ignored | (() => void);
      if (isIgnored(entry)) expect(entry.ignore.length, kind).toBeGreaterThan(10);
      else expect(typeof entry, kind).toBe("function");
    }
  });

  it("has an invalidation row for every kind, naming only real methods", () => {
    expect(Object.keys(EVENT_INVALIDATIONS).sort()).toEqual([...EVENT_KINDS].sort());
    for (const methods of Object.values(EVENT_INVALIDATIONS)) {
      for (const method of methods) expect(Object.keys(METHODS)).toContain(method);
    }
  });

  it("ignores kinds of a newer daemon and malformed payloads", () => {
    const { apply, stores } = setup();
    const before = stores.services.getState();
    expect(apply([event("future.kind", { v: 9 }), event("service.running", null)])).toBe(0);
    expect(stores.services.getState()).toBe(before);
  });

  it("isolates a reducer that throws", () => {
    const { apply, stores } = setup();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const registry = EVENT_REDUCERS as unknown as Record<string, unknown>;
    const original = registry["site.created"];
    registry["site.created"] = () => {
      throw new Error("boom");
    };
    let warnings = 0;
    try {
      apply([
        event("site.created", { v: 1, site_id: IDS.site2, host: "blog.test" }),
        service("service.running", IDS.mysql),
      ]);
      warnings = warn.mock.calls.length;
    } finally {
      registry["site.created"] = original;
      warn.mockRestore();
    }
    expect(stores.services.getState().instances[IDS.mysql]?.state).toBe("running");
    expect(warnings).toBe(1);
  });
});

describe("service reducers", () => {
  it("projects the state machine onto the instance", () => {
    const { apply, stores } = setup();
    apply([service("service.starting", IDS.mysql), service("service.running", IDS.mysql)]);
    const running = stores.services.getState().instances[IDS.mysql];
    expect(running?.state).toBe("running");
    expect(running?.pid).toBe(4412);
    expect(running?.started_at).toBe("2026-10-28T12:00:00Z");
    apply([service("service.stopped", IDS.mysql)]);
    const stopped = stores.services.getState().instances[IDS.mysql];
    expect([stopped?.state, stopped?.pid, stopped?.started_at]).toEqual(["stopped", null, null]);
  });

  it("keeps the diagnosis of a failure for describeError and the attention inputs", () => {
    const { apply, stores } = setup();
    const reason: T.FailureReasonDto = {
      kind: "port_in_use",
      port: 3306,
      pid: 77,
      exe: "mysqld.exe",
    };
    apply([
      service("service.failed", IDS.mysql, {
        error: { code: "port_in_use", message: "port 3306 is in use" },
        reason,
      }),
    ]);
    const failed = stores.services.getState().instances[IDS.mysql];
    expect(failed?.last_error?.code).toBe("port_in_use");
    expect(failed?.last_error?.details).toMatchObject({
      kind: "port_in_use",
      reason: "port_in_use",
      args: { port: 3306 },
    });
    expect(stores.attention.getState().serviceFailures[IDS.mysql]?.reason).toEqual(reason);
    apply([service("service.running", IDS.mysql)]);
    expect(stores.attention.getState().serviceFailures[IDS.mysql]).toBeUndefined();
    expect(stores.services.getState().instances[IDS.mysql]?.last_error).toBeNull();
  });

  it("asks for the services snapshot for an instance it does not know", () => {
    const { apply, refreshed } = setup();
    apply([service("service.running", "01JINST0000000000000000NEW")]);
    expect(refreshed).toEqual(["services"]);
  });

  it("clears the port conflicts of an instance once it runs", () => {
    const { apply, stores } = setup();
    apply([
      event("port.conflict", {
        v: 1,
        port: 3306,
        protocol: "tcp",
        owner_name: "mysql/default",
        instance_id: IDS.mysql,
        holder: { pid: 9, exe: "mysqld.exe" },
      } satisfies T.PortConflictPayload),
    ]);
    expect(Object.keys(stores.attention.getState().portConflicts)).toEqual(["tcp/3306"]);
    apply([service("service.running", IDS.mysql)]);
    expect(stores.attention.getState().portConflicts).toEqual({});
  });

  it("commits once per store for 1,000 events and keeps untouched entities identical", () => {
    const { apply, stores } = setup();
    const redis = stores.services.getState().instances[IDS.redis];
    const listener = vi.fn();
    const unsubscribe = stores.services.subscribe(listener);
    const events = Array.from({ length: 1000 }, (_, i) =>
      service(i % 2 === 0 ? "service.starting" : "service.running", IDS.mysql),
    );
    expect(apply(events)).toBe(1); // only the services store changed
    expect(listener).toHaveBeenCalledTimes(1);
    expect(stores.services.getState().instances[IDS.redis]).toBe(redis);
    expect(stores.services.getState().instances[IDS.mysql]?.state).toBe("running");
    unsubscribe();
  });

  it("updates pool workers on pool.scaled", () => {
    const { apply, stores, refreshed } = setup();
    apply([
      event("pool.scaled", {
        v: 1,
        version_id: IDS.php83,
        series: "8.3",
        from_workers: 2,
        to_workers: 4,
      }),
      event("pool.scaled", {
        v: 1,
        version_id: "01JVERS00000000000000PH840",
        series: "8.4",
        from_workers: 0,
        to_workers: 1,
      }),
    ]);
    expect(stores.services.getState().pools[IDS.php83]?.workers).toBe(4);
    expect(refreshed).toEqual(["services"]);
  });
});

describe("job reducers", () => {
  it("follows started → progress → done and never leaves a terminal state", () => {
    const { apply, stores } = setup();
    const job = "01JJOB00000000000000000001";
    apply([
      event("job.started", {
        v: 1,
        job_id: job,
        name: "tools.install",
        request_id: "01JREQ00000000000000000001",
      }),
      event("job.progress", { v: 1, job_id: job, pct: 40, message: "downloading" }),
    ]);
    expect(stores.jobs.getState().jobs[job]).toMatchObject({
      state: "running",
      name: "tools.install",
      request_id: "01JREQ00000000000000000001",
      progress: { pct: 40, message: "downloading" },
    });
    apply([
      event("job.done", {
        v: 1,
        job_id: job,
        name: "tools.install",
        duration_ms: 5,
        result: { ok: true },
      }),
    ]);
    apply([event("job.progress", { v: 1, job_id: job, pct: 90 })]);
    expect(stores.jobs.getState().jobs[job]).toMatchObject({ state: "done", result: { ok: true } });
  });

  it("records failures and cancellations", () => {
    const { apply, stores } = setup();
    apply([
      event("job.failed", {
        v: 1,
        job_id: "01JJOB00000000000000000002",
        name: "sites.add",
        duration_ms: 1,
        error: { code: "conflict", message: "x" },
      }),
      event("job.cancelled", {
        v: 1,
        job_id: "01JJOB00000000000000000003",
        name: "tools.install",
        duration_ms: 1,
        forced: false,
      }),
    ]);
    const jobs = stores.jobs.getState().jobs;
    expect(jobs["01JJOB00000000000000000002"]?.error?.code).toBe("conflict");
    expect(jobs["01JJOB00000000000000000003"]?.state).toBe("cancelled");
  });
});

describe("tool, site and system reducers", () => {
  it("replaces the pending updates with each tool.updates_available", () => {
    const { apply, stores } = setup();
    const update = { tool: "php", series: "8.3", from: "8.3.36", to: "8.3.37" };
    apply([event("tool.updates_available", { v: 1, updates: [update], count: 1 })]);
    expect(stores.tools.getState().updateCount).toBe(1);
    apply([event("tool.updates_available", { v: 1, updates: [], count: 0 })]);
    expect(stores.tools.getState()).toMatchObject({ updates: [], updateCount: 0 });
  });

  it("moves the current version and tracks installs", () => {
    const { apply, stores, refreshed } = setup();
    apply([
      event("tool.current_changed", {
        v: 1,
        tool: "php",
        from: "8.3.36",
        to: "8.4.0",
        version_id: "01JVERS0000000000000PHP840",
        elapsed_us: 10,
      }),
    ]);
    const php = stores.tools.getState().tools.php;
    expect(php?.current).toBe("8.4.0");
    expect(php?.installed.map((v) => v.is_current)).toEqual([false, true]);
    const install: T.ToolInstallPayload = {
      v: 1,
      job_id: "01JJOB00000000000000000009",
      tool: "redis",
      version: "7.4",
      version_id: null,
      progress: { pct: 10 },
      error: null,
    };
    apply([
      event("tool.install.started", install),
      event("tool.install.progress", { ...install, progress: { pct: 50 } }),
    ]);
    expect(stores.tools.getState().installs["01JJOB00000000000000000009"]?.progress?.pct).toBe(50);
    apply([event("tool.install.completed", install)]);
    expect(stores.tools.getState().installs).toEqual({});
    expect(refreshed).toEqual(["tools"]);
  });

  it("removes sites by event and refetches what the payload cannot build", () => {
    const { apply, stores, refreshed } = setup();
    apply([event("site.created", { v: 1, site_id: IDS.site2, host: "blog.test" })]);
    expect(refreshed).toEqual(["sites"]);
    apply([event("site.removed", { v: 1, site_id: IDS.site, host: "loja.test" })]);
    expect(stores.sites.getState().sites).toEqual({});
  });

  it("keeps the attention inputs (drift, actors, case, helper)", () => {
    const { apply, stores } = setup();
    const file = {
      v: 1,
      owner: "nginx/default",
      path: "etc/nginx/nginx.conf",
      fs_path: "etc/nginx/nginx.conf",
    };
    apply([
      event("config.drift_detected", file),
      event("actor.failed", { v: 1, actor: "dns-server", reason: "panic", restart_count: 5 }),
      event("fs.case_sensitivity_unavailable", {
        v: 1,
        dir: "www",
        site_id: null,
        reason: "fat32",
        reason_code: "fs",
      }),
      event("helper.op", { v: 1, name: "nrpt_add", ok: false, error: "denied" }),
    ]);
    const attention = stores.attention.getState();
    expect(Object.keys(attention.configDrift)).toEqual([file.path]);
    expect(Object.keys(attention.actorFailures)).toEqual(["dns-server"]);
    expect(Object.keys(attention.caseUnavailable)).toEqual(["www"]);
    expect(Object.keys(attention.helperFailures)).toEqual(["nrpt_add"]);
    apply([
      event("config.rendered", file),
      event("fs.case_sensitivity_applied", {
        v: 1,
        dir: "www",
        site_id: null,
        reason: null,
        reason_code: null,
      }),
      event("helper.op", { v: 1, name: "nrpt_add", ok: true, error: null }),
    ]);
    const after = stores.attention.getState();
    expect([after.configDrift, after.caseUnavailable, after.helperFailures]).toEqual([{}, {}, {}]);
  });

  it("resyncs on daemon.ready and records the shutdown reason", () => {
    const { apply, stores, resyncs } = setup();
    apply([event("daemon.shutdown", { v: 1, reason: "user" })]);
    expect(stores.connection.getState().shutdownReason).toBe("user");
    apply([event("daemon.ready", { v: 1, version: "0.2.0", api_version: { major: 1, minor: 1 } })]);
    expect(stores.connection.getState()).toMatchObject({
      daemonVersion: "0.2.0",
      shutdownReason: null,
    });
    expect(resyncs).toEqual(["daemon_ready"]);
  });

  it("reduces the E3 events (ADR-0040) and leaves the rest to queries", () => {
    const { apply, stores, refreshed } = setup();
    apply([
      event("port.conflict", {
        v: 1,
        port: 80,
        protocol: "tcp",
        owner_name: "nginx/default",
        instance_id: null,
        holder: { pid: 4412, exe: "httpd.exe" },
      } satisfies T.PortConflictPayload),
    ]);
    expect(Object.keys(stores.attention.getState().portConflicts)).toEqual(["tcp/80"]);
    const reduced = apply([
      event("port.holder_killed", {
        v: 1,
        port: 80,
        pid: 4412,
        exe: "httpd.exe",
        client: "Forgebox.exe",
      } satisfies T.PortHolderKilledPayload),
      event("tls.ca_untrusted", {
        v: 1,
        scope: "current_user",
        thumbprint_sha1: "AB".repeat(20),
      } satisfies T.TlsCaUntrustedPayload),
      event("mail.changed", {
        v: 1,
        instance_id: IDS.mysql,
        messages: 3,
        unread: 1,
        new_messages: 1,
      } satisfies T.MailChangedPayload),
      event("terminal.exited", {
        v: 1,
        terminal_id: "01JTERM0000000000000000001",
        exit_code: 0,
      } satisfies T.TerminalExitedPayload),
      event("config.changed", { v: 1, keys: ["tld"] } satisfies T.ConfigChangedPayload),
    ]);
    expect(stores.attention.getState().portConflicts).toEqual({});
    expect(refreshed).toEqual(["ca"]);
    expect(reduced).toBeGreaterThan(0);
    expect(EVENT_INVALIDATIONS["mail.changed"]).toEqual(["mail.status", "mail.list"]);
    expect(EVENT_INVALIDATIONS["terminal.exited"]).toEqual(["terminal.list"]);
    expect(EVENT_INVALIDATIONS["config.changed"]).toContain("system.config_list");
  });

  it("keeps certificates and DNS policies by key", () => {
    const { apply, stores, refreshed } = setup();
    apply([
      event("tls.leaf_issued", {
        v: 1,
        host: "loja.test",
        site_id: IDS.site,
        thumbprint_sha256: "AA",
        not_after: "2027-01-01T00:00:00Z",
      }),
      event("dns.policy_applied", { v: 1, tld: "test", policy: "applied" }),
    ]);
    expect(stores.system.getState().certificates["loja.test"]?.not_after).toBe(
      "2027-01-01T00:00:00Z",
    );
    expect(stores.system.getState().dnsPolicies.test?.policy).toBe("applied");
    expect(refreshed).toContain("dns");
  });
});
