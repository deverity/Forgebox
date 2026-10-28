import { describe, expect, expectTypeOf, it } from "vitest";
import { ForgeApi, isUlid, newIntent, ulid, type JobResultOf, type ResultOf } from "./api";
import { ForgeApiError, isForgeApiError } from "./errors";
import { createFakeBridge, fakeError, fakeJob } from "./fakeBridge";
import type * as T from "./generated";

async function rejection(promise: Promise<unknown>): Promise<ForgeApiError> {
  try {
    await promise;
  } catch (error) {
    if (isForgeApiError(error)) return error;
    throw error;
  }
  throw new Error("expected a rejection");
}

describe("ulid", () => {
  it("is 26 Crockford characters that sort by time", () => {
    const a = ulid(1_700_000_000_000);
    const b = ulid(1_700_000_000_001);
    expect(isUlid(a)).toBe(true);
    expect(a).toHaveLength(26);
    expect(a < b).toBe(true);
    expect(ulid()).not.toBe(ulid());
    expect(isUlid("not-a-ulid")).toBe(false);
    // The timestamp part decodes like the Rust `ulid` crate: 0 → all zeros.
    expect(ulid(0, (bytes) => bytes.fill(0))).toBe("0".repeat(26));
  });
});

describe("ForgeApi.call", () => {
  it("returns the typed result of the method", async () => {
    const fake = createFakeBridge({
      handlers: { "sites.park": (params) => ({ dir: params.dir, tld: ".test", sites: 2 }) },
    });
    const api = new ForgeApi(fake);
    const parked = await api.call("sites.park", { dir: "D:/clientes" });
    expectTypeOf(parked).toEqualTypeOf<ResultOf<"sites.park">>();
    expect(parked).toEqual({ dir: "D:/clientes", tld: ".test", sites: 2 });
    expect(fake.calls).toHaveLength(1);
    expect(isUlid(fake.calls[0]?.requestId ?? "")).toBe(true);
  });

  it("rejects with ForgeApiError carrying the code, details, method and request_id", async () => {
    const fake = createFakeBridge({
      handlers: {
        "sites.park": () =>
          fakeError("conflict", "already parked", "already_parked", { dir: "D:/x" }),
      },
    });
    const api = new ForgeApi(fake);
    const intent = newIntent();
    const error = await rejection(api.call("sites.park", { dir: "D:/x" }, { intent }));
    expect(error).toBeInstanceOf(ForgeApiError);
    expect(error.code).toBe("conflict");
    expect(error.reason).toBe("already_parked");
    expect(error.args).toEqual({ dir: "D:/x" });
    expect(error.method).toBe("sites.park");
    expect(error.requestId).toBe(intent.requestId);
  });

  it("reuses the intent's request_id on a retry, so the daemon acts once", async () => {
    let executions = 0;
    const fake = createFakeBridge({
      handlers: {
        "sites.park": (params) => {
          executions += 1;
          return { dir: params.dir, tld: ".test", sites: executions };
        },
      },
    });
    const api = new ForgeApi(fake);
    const intent = newIntent();
    const first = await api.call("sites.park", { dir: "D:/a" }, { intent });
    const retry = await api.call("sites.park", { dir: "D:/a" }, { intent });
    expect(retry).toEqual(first);
    expect(executions).toBe(1);
    expect(fake.calls.map((c) => c.requestId)).toEqual([intent.requestId, intent.requestId]);
    // A new intent acts again.
    const other = await api.call("sites.park", { dir: "D:/a" });
    expect(other.sites).toBe(2);
  });

  it("refuses what is not in METHODS before the daemon sees it", async () => {
    const fake = createFakeBridge();
    const api = new ForgeApi(fake);
    const error = await rejection(api.call("nope.nope" as "sites.list", {}));
    expect([error.code, error.reason]).toEqual(["not_found", "unknown_method"]);
    expect(fake.calls).toHaveLength(0);
  });

  it("reports a daemon that cannot be reached as `disconnected`", async () => {
    const api = new ForgeApi(createFakeBridge({ status: "stopped" }));
    const error = await rejection(api.call("system.ping", {}));
    expect([error.code, error.reason]).toEqual(["disconnected", "daemon_unreachable"]);
  });
});

describe("ForgeApi.job", () => {
  it("answers the JobDto at once and resolves `done` with the job result", async () => {
    const fake = createFakeBridge({
      handlers: {
        "tools.remove": () =>
          fakeJob({
            steps: [{ pct: 25 }, { pct: 75, message: "removing" }],
            outcome: { done: { ok: true, message: "removed" } },
          }),
      },
    });
    const api = new ForgeApi(fake);
    const progress: (number | null | undefined)[] = [];
    const handle = await api.job(
      "tools.remove",
      { spec: "php@8.3" },
      { onProgress: (p) => progress.push(p.pct) },
    );
    expectTypeOf(handle.done).resolves.toEqualTypeOf<JobResultOf<"tools.remove">>();
    expectTypeOf(handle.job).toEqualTypeOf<T.JobDto>();
    expect(handle.job.state).toBe("running");
    expect(handle.job.name).toBe("tools.remove");
    await expect(handle.done).resolves.toEqual({ ok: true, message: "removed" });
    expect(progress.filter((p) => p !== undefined && p !== null)).toEqual([25, 75]);
    // The job's own event stream is closed afterwards.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(fake.openStreams()).toBe(0);
  });

  it("rejects `done` with the job's error code", async () => {
    const fake = createFakeBridge({
      handlers: {
        "tools.remove": () =>
          fakeJob({
            outcome: {
              failed: {
                code: "port_in_use",
                message: "port 80 is taken",
                details: { reason: "port_in_use", args: { port: 80 } },
              },
            },
          }),
      },
    });
    const api = new ForgeApi(fake);
    const handle = await api.job("tools.remove", { spec: "nginx" });
    const error = await rejection(handle.done);
    expect(error.code).toBe("port_in_use");
    expect(error.args).toEqual({ port: 80 });
    expect(error.method).toBe("tools.remove");
  });

  it("sees a job that ended before its answer arrived", async () => {
    // The fake publishes `job.started` before answering and ends on the next tick.
    const fake = createFakeBridge({
      handlers: { "tools.remove": () => ({ ok: true, message: null }) },
    });
    const api = new ForgeApi(fake, "local", { jobPollMs: 60_000 });
    const handle = await api.job("tools.remove", { spec: "php" });
    await expect(handle.done).resolves.toEqual({ ok: true, message: null });
  });

  it("asks the daemon to cancel when aborted", async () => {
    const fake = createFakeBridge({ handlers: { "tools.remove": () => fakeJob({ hold: true }) } });
    const api = new ForgeApi(fake);
    const abort = new AbortController();
    const handle = await api.job("tools.remove", { spec: "php" }, { signal: abort.signal });
    abort.abort();
    const error = await rejection(handle.done);
    expect(error.code).toBe("cancelled");
    expect(fake.calls.map((c) => c.method)).toContain("jobs.cancel");
    expect(fake.jobList()[0]?.state).toBe("cancelled");
  });

  it("follows the job by polling when no event stream can be opened", async () => {
    const fake = createFakeBridge({
      // Events cannot be subscribed (as if the stream were refused): only the poll is left.
      notImplemented: ["events.subscribe"],
      jobStepMs: 30,
      handlers: {
        "tools.remove": () => fakeJob({ outcome: { done: { ok: true, message: "polled" } } }),
      },
    });
    const api = new ForgeApi(fake, "local", { jobPollMs: 20 });
    const handle = await api.job("tools.remove", { spec: "php" });
    await expect(handle.done).resolves.toEqual({ ok: true, message: "polled" });
    expect(fake.calls.some((c) => c.method === "jobs.get")).toBe(true);
  });

  it("does not start anything when the call is refused", async () => {
    const fake = createFakeBridge({ hello: { role: "viewer" } });
    const api = new ForgeApi(fake);
    const error = await rejection(api.job("tools.remove", { spec: "php" }));
    expect(error.code).toBe("forbidden");
    expect(fake.jobList()).toHaveLength(0);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(fake.openStreams()).toBe(0);
  });
});

describe("ForgeApi.subscribe", () => {
  it("delivers typed entries", async () => {
    const fake = createFakeBridge();
    const api = new ForgeApi(fake);
    const kinds: string[] = [];
    const stream = await api.subscribe(
      "events.subscribe",
      { filter: { kinds: ["site.*"] } },
      (entries) => {
        for (const entry of entries) {
          if (entry.kind === "item") {
            expectTypeOf(entry.item).toEqualTypeOf<T.EventEnvelope>();
            kinds.push(entry.item.event.kind);
          }
        }
      },
    );
    fake.publish("site.created", { v: 1 });
    fake.publish("service.running", { v: 1 });
    fake.flush();
    expect(kinds).toEqual(["site.created"]);
    await stream.unsubscribe();
  });
});
