// Mutations the daemon reports only through `job.*` (no entity event): `services.create`
// without autostart, `services.remove`, `tools.remove`... The `JOB_EFFECTS` row of the job's
// method refetches the snapshots it touches (once, debounced) and invalidates its queries.
import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type * as T from "../ipc/generated";
import { METHODS, type MethodMeta } from "../ipc/methods";
import { JOB_EFFECTS, applyMutationEffect, jobMethod } from "./effects";
import { forgeQuery } from "./queries";
import { ForgeStateProvider, useInstances, useTools } from "./react";
import { commitTx, openTx, reduceEvents } from "./reducers";
import { SLICES, SNAPSHOTS } from "./snapshots";
import { createContextStores, type SliceName } from "./stores";
import {
  IDS,
  instance,
  populatedData,
  startHarness,
  tool,
  type Harness,
  type ScenarioData,
} from "./testFixtures";

const LEGACY = "01JINST0000000000000LEGACY";
const PHP836 = "01JVERS0000000000000PHP836";
const PHP840 = "01JVERS0000000000000PHP840";
const SNAPSHOT_METHODS = SLICES.map((slice) => SNAPSHOTS[slice].method);

function version(id: string, v: string, current: boolean): T.ToolVersionDto {
  return {
    id,
    tool: "php",
    version: v,
    canonical_name: `${v}-nts-vs17-x64`,
    variant: {},
    state: "installed",
    source: "registry",
    path: `C:/Forgebox/bin/php/${v}`,
    size_bytes: null,
    installed_at: null,
    is_current: current,
  };
}

const sleep = (ms: number) =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

let harness: Harness | null = null;
afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

function snapshotCalls(h: Harness): Record<string, number> {
  return Object.fromEntries(SNAPSHOT_METHODS.map((method) => [method, h.count(method)]));
}

/** Snapshot calls made since `before`, without the zeros. */
function refetched(h: Harness, before: Record<string, number>): Record<string, number> {
  return Object.fromEntries(
    Object.entries(snapshotCalls(h))
      .map(([method, n]) => [method, n - (before[method] ?? 0)] as const)
      .filter(([, n]) => n !== 0),
  );
}

/** Lets the fake end its jobs, delivers the frames, applies them and waits for snapshots. */
async function settle(h: Harness): Promise<void> {
  await act(async () => {
    await sleep(5); // the fake ends a job on a 0 ms timer
    h.fake.flush();
    await sleep(20);
    h.runtime.flushEvents();
    await h.runtime.settled();
    await sleep(20); // the query invalidation debounce (10 ms in the harness)
  });
}

function InstanceList() {
  const instances = useInstances();
  return (
    <ul>
      {Object.values(instances).map((i) => (
        <li key={i.id} data-testid="instance">{`${i.tool}/${i.name}`}</li>
      ))}
    </ul>
  );
}

function PhpVersions() {
  const php = useTools((state) => state.tools.php);
  return (
    <ul>
      {(php?.installed ?? []).map((v) => (
        <li key={v.id} data-testid="version">
          {v.version}
        </li>
      ))}
    </ul>
  );
}

function texts(testId: string): string[] {
  return screen.queryAllByTestId(testId).map((node) => node.textContent);
}

function phpData(): ScenarioData {
  const data = populatedData();
  data.tools = [
    tool("php", {
      current: "8.3.36",
      installed: [version(PHP836, "8.3.36", true), version(PHP840, "8.4.0", false)],
    }),
  ];
  return data;
}

describe("JOB_EFFECTS registry", () => {
  it("has a row for every job method and every mutating call, naming real methods and slices", () => {
    const expected = Object.entries(METHODS as Record<string, MethodMeta>)
      .filter(([, meta]) => meta.kind === "job" || meta.mutating)
      .map(([method]) => method)
      .sort();
    expect(Object.keys(JOB_EFFECTS).sort()).toEqual(expected);
    for (const [method, row] of Object.entries(JOB_EFFECTS)) {
      for (const slice of row.refresh) expect(SLICES, method).toContain(slice);
      for (const query of row.invalidate) expect(Object.keys(METHODS), method).toContain(query);
    }
  });

  it("covers the mutations that publish no entity event", () => {
    expect(JOB_EFFECTS["services.create"].refresh).toEqual(["services"]);
    expect(JOB_EFFECTS["services.remove"].refresh).toEqual(["services"]);
    expect(JOB_EFFECTS["services.set"].refresh).toEqual(["services"]);
    for (const method of [
      "tools.install",
      "tools.remove",
      "tools.rollback",
      "tools.use",
    ] as const) {
      expect(JOB_EFFECTS[method].refresh, method).toEqual(["tools", "services"]);
      expect(JOB_EFFECTS[method].invalidate, method).toContain("tools.info");
    }
    expect(JOB_EFFECTS["services.set"].invalidate).toContain("services.ports");
    for (const method of ["sites.add", "sites.remove", "sites.php", "sites.alias"] as const) {
      expect(JOB_EFFECTS[method].refresh, method).toContain("sites");
    }
  });

  it("takes the method from the job name up to the first space", () => {
    expect(jobMethod("services.remove mysql/legacy")).toBe("services.remove");
    expect(jobMethod("tools.install php@8.4 redis")).toBe("tools.install");
    expect(jobMethod("tls.renew")).toBe("tls.renew");
    expect(jobMethod("future.method x")).toBeNull();
    expect(jobMethod("services.status")).toBeNull(); // a read has no row
    expect(jobMethod("")).toBeNull();
  });

  it("applies the row from job.done, job.failed and job.cancelled, even after the poll ended it", () => {
    const stores = createContextStores("local");
    const refreshed: SliceName[] = [];
    const invalidated: string[] = [];
    const apply = (events: T.EventDto[]) => {
      const tx = openTx(stores, {
        refresh: (slice) => refreshed.push(slice),
        invalidate: (methods) => invalidated.push(...methods),
        resync: () => undefined,
      });
      reduceEvents(tx, events);
      commitTx(tx);
    };
    const base = { v: 1, duration_ms: 1 };
    let seq = 0;
    const event = (kind: string, payload: unknown): T.EventDto => {
      seq += 1;
      return {
        id: `01JEVENT${String(seq).padStart(18, "0")}`,
        seq,
        ts: "2026-10-28T12:00:00Z",
        kind,
        payload,
      };
    };
    // Already terminal in the store (the jobs.get poll saw it first).
    stores.jobs.setState({
      jobs: {
        J1: {
          id: "J1",
          name: "services.remove mysql/legacy",
          request_id: null,
          state: "done",
          progress: {},
          cancel_requested: false,
          started_at: "2026-10-28T11:59:59Z",
          finished_at: "2026-10-28T12:00:00Z",
          result: null,
          error: null,
        },
      },
    });
    apply([
      event("job.done", {
        ...base,
        job_id: "J1",
        name: "services.remove mysql/legacy",
        result: null,
      }),
    ]);
    expect(refreshed).toEqual(["services"]);
    expect(invalidated).toContain("services.ports");
    apply([
      event("job.failed", {
        ...base,
        job_id: "J2",
        name: "tools.remove php@8.4",
        error: { code: "io", message: "x" },
      }),
      event("job.cancelled", { ...base, job_id: "J3", name: "sites.add blog.test", forced: false }),
      event("job.done", { ...base, job_id: "J4", name: "daemon.doctor", result: null }),
      event("job.done", { ...base, job_id: "J5", name: "future.method", result: null }),
    ]);
    expect(refreshed).toEqual(["services", "tools", "services", "sites", "services"]);
    expect(stores.jobs.getState().jobs.J1?.state).toBe("done");
  });

  it("applyMutationEffect ignores methods without a row", () => {
    const sink = { refresh: vi.fn(), invalidate: vi.fn() };
    expect(applyMutationEffect(sink, "services.status")).toBe(false);
    expect(applyMutationEffect(sink, "tools.policy")).toBe(true);
    expect(sink.refresh).toHaveBeenCalledWith("tools");
  });
});

describe("stores after mutations without entity events", () => {
  it("services.create with autostart=false: the new instance shows up after job.done", async () => {
    const data = populatedData();
    harness = await startHarness({
      data,
      scenario: {
        handlers: {
          "services.create": (params) => {
            const created = instance(LEGACY, `mysql/${params.name}`, { autostart: false });
            data.instances.push(created);
            return created;
          },
        },
      },
    });
    const h = harness;
    render(
      <ForgeStateProvider layer={h.layer}>
        <InstanceList />
      </ForgeStateProvider>,
    );
    expect(texts("instance")).toEqual(["mysql/default", "redis/default"]);
    const before = snapshotCalls(h);
    await act(async () => {
      await h.runtime.api.call("services.create", {
        tool: "mysql@8.4",
        name: "legacy",
        autostart: false,
      });
    });
    await settle(h);
    expect(texts("instance")).toEqual(["mysql/default", "redis/default", "mysql/legacy"]);
    expect(refetched(h, before)).toEqual({ "services.status": 1 });
  });

  it("services.remove: the instance is gone after job.done (three removals → one refetch)", async () => {
    const data = populatedData();
    data.instances.push(instance(LEGACY, "mysql/legacy"));
    harness = await startHarness({
      data,
      scenario: {
        handlers: {
          "services.remove": (params) => {
            data.instances = data.instances.filter(
              (i) => `${i.tool}/${i.name}` !== params.instance,
            );
            return { ok: true, message: null };
          },
        },
      },
    });
    const h = harness;
    render(
      <ForgeStateProvider layer={h.layer}>
        <InstanceList />
      </ForgeStateProvider>,
    );
    expect(texts("instance")).toEqual(["mysql/default", "redis/default", "mysql/legacy"]);
    const before = snapshotCalls(h);
    await act(async () => {
      await h.runtime.api.call("services.remove", { instance: "mysql/legacy", confirm: true });
    });
    await settle(h);
    expect(texts("instance")).toEqual(["mysql/default", "redis/default"]);
    expect(h.runtime.stores.services.getState().instances[LEGACY]).toBeUndefined();
    expect(refetched(h, before)).toEqual({ "services.status": 1 });

    // Several jobs ending together still refetch once (debounced).
    const again = snapshotCalls(h);
    await act(async () => {
      await Promise.all([
        h.runtime.api.call("services.remove", { instance: "mysql/default", confirm: true }),
        h.runtime.api.call("services.remove", { instance: "redis/default", confirm: true }),
        h.runtime.api.call("services.remove", { instance: "nope/default", confirm: true }),
      ]);
    });
    await settle(h);
    expect(texts("instance")).toEqual([]);
    expect(refetched(h, again)).toEqual({ "services.status": 1 });
  });

  it("tools.remove: the version is gone after job.done and tools.info is invalidated", async () => {
    const data = phpData();
    harness = await startHarness({
      data,
      scenario: {
        handlers: {
          "tools.remove": (params) => {
            const [id, v] = params.spec.split("@");
            data.tools = data.tools.map((t) =>
              t.id === id ? { ...t, installed: t.installed.filter((x) => x.version !== v) } : t,
            );
            return { ok: true, message: null };
          },
          "tools.info": () => ({ tool: data.tools[0] ?? tool("php"), available: [], used_by: [] }),
        },
      },
    });
    const h = harness;
    render(
      <ForgeStateProvider layer={h.layer}>
        <PhpVersions />
      </ForgeStateProvider>,
    );
    expect(texts("version")).toEqual(["8.3.36", "8.4.0"]);
    const info = forgeQuery(h.runtime.api, "tools.info", { tool: "php" });
    await act(async () => {
      await h.layer.queryClient.query(info);
    });
    expect(h.layer.queryClient.getQueryState(info.queryKey)?.isInvalidated).toBe(false);
    const before = snapshotCalls(h);
    await act(async () => {
      await h.runtime.api.call("tools.remove", { spec: "php@8.4.0", confirm: true });
    });
    await settle(h);
    expect(texts("version")).toEqual(["8.3.36"]);
    // tools (the version) + services (the pools a removed PHP version stops).
    expect(refetched(h, before)).toEqual({ "tools.list": 1, "services.status": 1 });
    expect(h.layer.queryClient.getQueryState(info.queryKey)?.isInvalidated).toBe(true);
  });

  it("a mutating call through the reconnect queue applies its row when it resolves", async () => {
    const data = phpData();
    harness = await startHarness({
      data,
      scenario: {
        handlers: {
          "tools.use": (params) => {
            data.tools = data.tools.map((t) =>
              t.id === params.tool
                ? {
                    ...t,
                    current: params.version,
                    installed: t.installed.map((x) => ({
                      ...x,
                      is_current: x.version === params.version,
                    })),
                  }
                : t,
            );
            return version(PHP840, params.version, true);
          },
        },
      },
    });
    const h = harness;
    const before = snapshotCalls(h);
    await act(async () => {
      await h.runtime.queue.call("tools.use", { tool: "php", version: "8.4.0" });
    });
    await settle(h);
    expect(h.runtime.stores.tools.getState().tools.php?.current).toBe("8.4.0");
    expect(refetched(h, before)).toEqual({ "tools.list": 1, "services.status": 1 });

    // A read through the queue applies nothing.
    const reads = snapshotCalls(h);
    await act(async () => {
      await h.runtime.queue.call("services.ports", {}).catch(() => undefined);
    });
    await settle(h);
    expect(refetched(h, reads)).toEqual({});
  });

  it("a job started by another client (the CLI) refreshes the stores too", async () => {
    const data = populatedData();
    harness = await startHarness({ data });
    const h = harness;
    const before = snapshotCalls(h);
    data.instances = data.instances.filter((i) => i.id !== IDS.redis);
    // The daemon's real job names carry the arguments.
    h.fake.publish("job.started", {
      v: 1,
      job_id: "01JJOB0000000000000000CLI1",
      name: "services.remove redis/default",
      request_id: null,
    });
    h.fake.publish("job.done", {
      v: 1,
      job_id: "01JJOB0000000000000000CLI1",
      name: "services.remove redis/default",
      duration_ms: 3,
      result: { ok: true, message: null },
    });
    await settle(h);
    expect(Object.keys(h.runtime.stores.services.getState().instances)).toEqual([IDS.mysql]);
    expect(refetched(h, before)).toEqual({ "services.status": 1 });
  });
});
