import { describe, expect, it } from "vitest";
import { ulid } from "./api";
import { LOCAL_CONTEXT, type ContextStateDto, type StreamEntry, type StreamFrame } from "./bridge";
import { ForgeApiError, isForgeApiError } from "./errors";
import {
  NOT_IMPLEMENTED,
  SCENARIOS,
  createFakeBridge,
  fakeJob,
  registerScenario,
  scenarioFromSearch,
  type FakeBridge,
} from "./fakeBridge";
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

function call(fake: FakeBridge, method: string, params: unknown = {}, requestId = ulid()) {
  return fake.call({ context: LOCAL_CONTEXT, method, params, request_id: requestId });
}

/** Subscribes and records every frame. */
async function record(fake: FakeBridge, method: string, params: unknown) {
  const frames: StreamFrame[] = [];
  const handle = await fake.subscribe(LOCAL_CONTEXT, method, params, (frame) => frames.push(frame));
  const entries = () => frames.flatMap((f) => f.entries);
  const seqs = () =>
    entries().flatMap((e: StreamEntry) =>
      e.kind === "item" ? [(e.item as { seq: number }).seq] : [],
    );
  const kinds = () =>
    entries().flatMap((e: StreamEntry) =>
      e.kind === "item" ? [(e.item as T.EventEnvelope).event.kind] : [],
    );
  const missed = () => entries().reduce((n, e) => n + (e.kind === "lagged" ? e.missed : 0), 0);
  return { handle, frames, entries, seqs, kinds, missed };
}

describe("fakeBridge jobs", () => {
  it("answer a running JobDto, then job.started → progress → done in seq order", async () => {
    const fake = createFakeBridge({
      handlers: {
        "tools.remove": () =>
          fakeJob({
            steps: [{ pct: 10 }, { pct: 90 }],
            outcome: { done: { ok: true, message: null } },
          }),
      },
    });
    const events = await record(fake, "events.subscribe", { filter: { kinds: ["job.*"] } });
    const job = (await call(fake, "tools.remove", { spec: "php" })) as T.JobDto;
    expect(job.state).toBe("running");
    expect(job.result).toBeNull();
    await new Promise((resolve) => setTimeout(resolve, 20));
    fake.flush();
    expect(events.kinds()).toEqual(["job.started", "job.progress", "job.progress", "job.done"]);
    const seqs = events.seqs();
    expect([...seqs].sort((a, b) => a - b)).toEqual(seqs);
    const snapshot = (await call(fake, "jobs.get", { job_id: job.id })) as T.JobDto;
    expect(snapshot.state).toBe("done");
    expect(snapshot.result).toEqual({ ok: true, message: null });
  });

  it("fail with the scripted error and can be cancelled while running", async () => {
    const fake = createFakeBridge({
      handlers: {
        "tools.remove": (params) =>
          params.spec === "hold"
            ? fakeJob({ hold: true })
            : fakeJob({ outcome: { failed: { code: "io", message: "disk full" } } }),
      },
    });
    const failing = (await call(fake, "tools.remove", { spec: "x" })) as T.JobDto;
    await new Promise((resolve) => setTimeout(resolve, 0));
    const failed = (await call(fake, "jobs.get", { job_id: failing.id })) as T.JobDto;
    expect([failed.state, failed.error?.code]).toEqual(["failed", "io"]);
    const held = (await call(fake, "tools.remove", { spec: "hold" })) as T.JobDto;
    const cancel = (await call(fake, "jobs.cancel", { job_id: held.id })) as T.JobCancelDto;
    expect(cancel.was_running).toBe(true);
    expect(fake.jobList().map((j) => j.state)).toEqual(["failed", "cancelled"]);
  });
});

describe("fakeBridge streams", () => {
  it("replay what follows resume_from exactly once, then go live", async () => {
    const fake = createFakeBridge();
    for (let i = 0; i < 5; i += 1) fake.publish("config.rendered", { v: 1, i });
    const events = await record(fake, "events.subscribe", { filter: {}, resume_from: 2 });
    expect(events.handle.resumePoint).toBe(5);
    fake.publish("config.rendered", { v: 1, i: 5 });
    fake.flush();
    expect(events.seqs()).toEqual([3, 4, 5, 6]);
    expect(events.missed()).toBe(0);
  });

  it("report history older than the daemon keeps as lagged", async () => {
    const fake = createFakeBridge({ historySize: 3 });
    for (let i = 0; i < 10; i += 1) fake.publish("config.rendered");
    const events = await record(fake, "events.subscribe", { resume_from: 0 });
    fake.flush();
    expect(events.entries()[0]).toEqual({ kind: "lagged", missed: 7 });
    expect(events.seqs()).toEqual([8, 9, 10]);
  });

  it("drop the oldest for a stalled consumer and say where (delivered + lost = published)", async () => {
    const fake = createFakeBridge({ streamCapacity: 100 });
    const events = await record(fake, "events.subscribe", {});
    fake.pauseStreams();
    for (let i = 0; i < 1000; i += 1) fake.publish("config.rendered");
    fake.resumeStreams();
    fake.flush();
    const seqs = events.seqs();
    expect(seqs.at(-1)).toBe(1000);
    expect(seqs).toHaveLength(100);
    expect(events.missed()).toBe(900);
    expect(events.entries()[0]).toEqual({ kind: "lagged", missed: 900 });
    expect(events.frames.every((f) => f.entries.length <= 256)).toBe(true);
    expect(events.frames.map((f) => f.frame)).toEqual(events.frames.map((_, i) => i + 1));
  });

  it("batch a burst in frames of at most 256 entries", async () => {
    const fake = createFakeBridge();
    const events = await record(fake, "events.subscribe", {});
    for (let i = 0; i < 1000; i += 1) fake.publish("config.rendered");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(events.frames.map((f) => f.entries.length)).toEqual([256, 256, 256, 232]);
    expect(events.seqs()).toEqual(Array.from({ length: 1000 }, (_, i) => i + 1));
  });

  it("resume after a reconnection without loss or duplicates", async () => {
    const fake = createFakeBridge();
    const events = await record(fake, "events.subscribe", {});
    fake.publish("a");
    fake.publish("b");
    fake.flush();
    fake.disconnect();
    fake.publish("c");
    fake.publish("d");
    fake.reconnect();
    fake.publish("e");
    fake.flush();
    expect(events.kinds()).toEqual(["a", "b", "c", "d", "e"]);
    expect(events.missed()).toBe(0);
  });

  it("say `reset` when the daemon's sequence restarts", async () => {
    const fake = createFakeBridge();
    const events = await record(fake, "events.subscribe", {});
    fake.publish("a");
    fake.publish("b");
    fake.resetStreams();
    fake.publish("c");
    fake.flush();
    expect(
      events.entries().map((e) => (e.kind === "item" ? (e.item as { seq: number }).seq : e.kind)),
    ).toEqual([1, 2, "reset", 1]);
  });

  it("filter events by kind pattern and payload ids, logs by target", async () => {
    const fake = createFakeBridge();
    const jobs = await record(fake, "events.subscribe", {
      filter: { kinds: ["job.*"], job_id: "J1" },
    });
    const logs = await record(fake, "logs.subscribe", { target: "nginx" });
    fake.publish("job.progress", { job_id: "J1" });
    fake.publish("job.progress", { job_id: "J2" });
    fake.publish("site.created", { job_id: "J1" });
    fake.emitLog("nginx", { ts: null, source: "stdout", level: null, text: "up" });
    fake.emitLog("mysql/default", { ts: null, source: "stdout", level: null, text: "x" });
    fake.flush();
    expect(jobs.seqs()).toEqual([1]);
    const lines = logs
      .entries()
      .flatMap((e) => (e.kind === "item" ? [(e.item as T.LogEnvelope).line.text] : []));
    expect(lines).toEqual(["up"]);
  });

  it("wait for the daemon when subscribed while it is down", async () => {
    const fake = createFakeBridge({ status: "stopped" });
    const events = await record(fake, "events.subscribe", {});
    expect(events.handle.pending).toBe(true);
    await fake.start(LOCAL_CONTEXT);
    fake.publish("daemon.ready");
    fake.flush();
    expect(events.kinds()).toEqual(["daemon.ready"]);
    await events.handle.unsubscribe();
    expect(fake.openStreams()).toBe(0);
  });
});

describe("fakeBridge daemon semantics", () => {
  it("refuses local_only methods when hello.local is false", async () => {
    const fake = createFakeBridge(SCENARIOS.get("remote")?.());
    const error = await rejection(call(fake, "sites.open", { site: "loja.test", with: "browser" }));
    expect(error.code).toBe("forbidden");
    expect(error.message).toContain("local_only");
    // Other methods work on the remote.
    await expect(call(fake, "system.ping")).resolves.toHaveProperty("server_time");
    const state = await fake.state(LOCAL_CONTEXT);
    expect(state.hello?.local).toBe(false);
  });

  it("refuses by role like the daemon", async () => {
    const fake = createFakeBridge({ hello: { role: "viewer" } });
    const error = await rejection(call(fake, "sites.park", { dir: "D:/x" }));
    expect(error.code).toBe("forbidden");
    expect(error.message).toContain("requires role operator");
  });

  it("answers not_implemented for the not-implemented table, with the roadmap task", async () => {
    const fake = createFakeBridge();
    const error = await rejection(call(fake, "db.list", {}));
    expect(error.code).toBe("not_implemented");
    expect(error.reason).toBe("not_implemented");
    expect(error.args).toEqual({ method: "db.list", task: "M2-E3", what: "database management" });
    const state = await fake.state(LOCAL_CONTEXT);
    expect(state.hello?.not_implemented).toEqual(NOT_IMPLEMENTED.map((row) => row.method));
    // Streams too.
    const stream = await rejection(
      fake.subscribe(LOCAL_CONTEXT, "metrics.subscribe", {}, () => undefined),
    );
    expect(stream.code).toBe("not_implemented");
    // A scenario decides its own list.
    const none = createFakeBridge({
      notImplemented: [],
      handlers: { "db.list": () => ({ instances: [], databases: [] }) as never },
    });
    await expect(call(none, "db.list", {})).resolves.toBeDefined();
  });

  it("validates like the Rust bridge before anything reaches the daemon", async () => {
    const fake = createFakeBridge();
    const cases: [Promise<unknown>, string, string][] = [
      [call(fake, "nope.nope"), "not_found", "unknown_method"],
      [call(fake, "events.subscribe"), "invalid_input", "use_subscribe"],
      [
        call(fake, "logs.unsubscribe", { subscription_id: ulid() }),
        "invalid_input",
        "use_unsubscribe",
      ],
      [call(fake, "system.ping", [1]), "invalid_input", "params_not_object"],
      [call(fake, "system.ping", {}, "bad"), "invalid_input", "invalid_request_id"],
      [
        fake.call({ context: "hml", method: "system.ping", request_id: ulid() }),
        "not_found",
        "unknown_context",
      ],
      [
        fake.subscribe(LOCAL_CONTEXT, "sites.list", {}, () => undefined),
        "invalid_input",
        "not_a_stream",
      ],
    ];
    for (const [promise, code, reason] of cases) {
      const error = await rejection(promise);
      expect([error.code, error.reason]).toEqual([code, reason]);
    }
    expect(fake.calls).toHaveLength(0);
  });

  it("refuses a request_id reused for another method", async () => {
    const fake = createFakeBridge({
      handlers: {
        "sites.park": (p) => ({ dir: p.dir, tld: ".test", sites: 1 }),
        "sites.unpark": () => ({ ok: true, message: null }),
      },
    });
    const id = ulid();
    await call(fake, "sites.park", { dir: "D:/x" }, id);
    const error = await rejection(call(fake, "sites.unpark", { dir: "D:/x" }, id));
    expect(error.code).toBe("invalid_input");
  });

  it("flags a scenario without a handler instead of inventing an answer", async () => {
    const error = await rejection(call(createFakeBridge({ name: "t" }), "sites.list"));
    expect([error.code, error.reason]).toEqual(["internal", "fake_no_handler"]);
  });
});

describe("fakeBridge connection", () => {
  it("emits states with growing versions on disconnect, reconnect and stop", async () => {
    const fake = createFakeBridge();
    const seen: ContextStateDto[] = [];
    const unlisten = await fake.onConnection((state) => seen.push(state));
    fake.disconnect();
    fake.reconnect();
    fake.stop("user");
    unlisten();
    fake.disconnect();
    expect(seen.map((s) => s.status)).toEqual(["reconnecting", "connected", "stopped"]);
    expect(seen.map((s) => s.version)).toEqual([1, 2, 3]);
    expect(seen[2]?.autostart.stopped_reason).toBe("user");
    expect(seen[1]?.hello?.local).toBe(true);
  });

  it("starts an uninitialized root only when onboarding asks for it", async () => {
    const fake = createFakeBridge(SCENARIOS.get("uninitialized")?.());
    expect((await fake.state(LOCAL_CONTEXT)).status).toBe("uninitialized");
    const error = await rejection(fake.start(LOCAL_CONTEXT));
    expect(error.reason).toBe("root_uninitialized");
    const state = await fake.start(LOCAL_CONTEXT, { initialize: true });
    expect(state.status).toBe("connected");
    expect(state.autostart.initialized).toBe(true);
  });
});

describe("scenarios", () => {
  it("are chosen by ?scenario= and can be registered by features", () => {
    expect(scenarioFromSearch("?scenario=daemon-down").status).toBe("stopped");
    expect(scenarioFromSearch("?scenario=slow").latencyMs).toBe(1500);
    expect(scenarioFromSearch("?scenario=unknown").name).toBe("empty");
    expect(scenarioFromSearch("").name).toBe("empty");
    registerScenario("populated-test", () => ({ name: "populated-test", latencyMs: 1 }));
    expect(scenarioFromSearch("?scenario=populated-test").latencyMs).toBe(1);
    for (const name of [
      "empty",
      "daemon-down",
      "remote",
      "not-implemented",
      "slow",
      "errors",
      "uninitialized",
    ]) {
      expect(SCENARIOS.has(name), name).toBe(true);
    }
  });

  it("errors: every call without a handler fails with a stable code", async () => {
    const fake = createFakeBridge(SCENARIOS.get("errors")?.());
    const error = await rejection(call(fake, "sites.list"));
    expect([error.code, error.reason]).toEqual(["internal", "fake_error"]);
    await expect(call(fake, "system.ping")).resolves.toBeDefined();
  });

  it("not-implemented: domain methods answer not_implemented", async () => {
    const fake = createFakeBridge(SCENARIOS.get("not-implemented")?.());
    expect((await rejection(call(fake, "sites.list"))).code).toBe("not_implemented");
  });
});
