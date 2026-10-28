// Acceptance (M2-E1-T9 #3): useJob resolves through events, through the 1 s `jobs.get`
// safety poll when events are dropped, and `cancel()` ends in the cancelled state.
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeJob } from "../ipc/fakeBridge";
import { JOB_POLL_MS } from "./runtime";
import { ForgeStateProvider, useJob } from "./react";
import { startHarness, type Harness } from "./testFixtures";

let harness: Harness | null = null;
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
});

function wrapper(h: Harness) {
  return ({ children }: { children: ReactNode }) => (
    <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
  );
}

describe("useJob", () => {
  it("polls jobs.get every second by default", () => {
    expect(JOB_POLL_MS).toBe(1000);
  });

  it("resolves through job.* events", async () => {
    harness = await startHarness({
      scenario: {
        jobStepMs: 5,
        handlers: {
          "tools.remove": () =>
            fakeJob({
              steps: [{ pct: 50, message: "removing" }],
              outcome: { done: { ok: true, message: "removed" } },
            }),
        },
      },
      jobPollMs: 60_000, // no poll: only events can finish it
    });
    const { job } = await harness.runtime.api.job("tools.remove", { spec: "php@8.3" });
    const { result } = renderHook(() => useJob(job.id), { wrapper: wrapper(harness) });
    await waitFor(() => {
      expect(result.current.state).toBe("done");
    });
    expect(result.current.result).toEqual({ ok: true, message: "removed" });
    expect(result.current.error).toBeNull();
    expect(harness.count("jobs.get")).toBe(0);
  });

  it("resolves through the jobs.get poll when the events are dropped", async () => {
    vi.useFakeTimers();
    harness = await startHarness({
      scenario: {
        handlers: {
          "tools.remove": () =>
            fakeJob({ stepMs: 300, outcome: { done: { ok: true, message: "x" } } }),
        },
      },
    });
    harness.fake.pauseStreams(); // a stalled webview: no event reaches the stores
    // `call` (not `api.job`, which runs its own follower): only useJob polls here.
    const job = await harness.runtime.api.call("tools.remove", { spec: "php@8.3" });
    const { result } = renderHook(() => useJob(job.id), { wrapper: wrapper(harness) });
    expect(result.current.state).toBe("unknown");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(999);
    });
    expect(harness.count("jobs.get")).toBe(0);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(harness.count("jobs.get")).toBe(1);
    expect(result.current.state).toBe("done");
    expect(result.current.result).toEqual({ ok: true, message: "x" });
    // Terminal: the poll stops.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(harness.count("jobs.get")).toBe(1);
  });

  it("cancel() ends in the cancelled state (events)", async () => {
    harness = await startHarness({
      scenario: { handlers: { "tools.install": () => fakeJob({ hold: true }) } },
      jobPollMs: 60_000,
    });
    const { job } = await harness.runtime.api.job("tools.install", { specs: ["php@8.4"] });
    const { result } = renderHook(() => useJob(job.id), { wrapper: wrapper(harness) });
    await waitFor(() => {
      expect(result.current.state).toBe("running");
    });
    await act(async () => {
      await result.current.cancel();
    });
    await waitFor(() => {
      expect(result.current.state).toBe("cancelled");
    });
    expect(result.current.error?.code).toBe("cancelled");
    expect(harness.fake.calls.find((c) => c.method === "jobs.cancel")?.params).toEqual({
      job_id: job.id,
    });
  });

  it("cancel() ends in the cancelled state (poll, events dropped)", async () => {
    harness = await startHarness({
      scenario: { handlers: { "tools.install": () => fakeJob({ hold: true }) } },
      jobPollMs: 20,
    });
    harness.fake.pauseStreams();
    const { job } = await harness.runtime.api.job("tools.install", { specs: ["php@8.4"] });
    const { result } = renderHook(() => useJob(job.id), { wrapper: wrapper(harness) });
    await waitFor(() => {
      expect(result.current.state).toBe("running");
    });
    await act(async () => {
      await result.current.cancel();
    });
    await waitFor(() => {
      expect(result.current.state).toBe("cancelled");
    });
  });

  it("reports a job the daemon does not know", async () => {
    harness = await startHarness({ jobPollMs: 10 });
    const { result } = renderHook(() => useJob("01JJOB0000000000000000NONE"), {
      wrapper: wrapper(harness),
    });
    await waitFor(() => {
      expect(result.current.error?.code).toBe("not_found");
    });
    expect(result.current.state).toBe("unknown");
  });
});
