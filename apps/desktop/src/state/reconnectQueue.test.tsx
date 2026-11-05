// Acceptance (M2-E1-T9 #6): the reconnect queue sends after Connected with the same
// request_id and fails with a localized error after 30 s; capabilities mark local_only
// methods unavailable when hello.local=false.
import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { beforeAll, afterEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../i18n";
import { newIntent } from "../ipc/api";
import { describeError, translateError, type ErrorTranslate } from "../ipc/describeError";
import { ForgeApiError, isForgeApiError } from "../ipc/errors";
import { METHODS, type MethodMeta } from "../ipc/methods";
import { capabilityOf, capabilityTable } from "./capabilities";
import { ForgeStateProvider, useCapabilities, useCapability } from "./react";
import { RECONNECT_HOLD_MS } from "./reconnectQueue";
import { startHarness, type Harness } from "./testFixtures";

let harness: Harness | null = null;
beforeAll(async () => {
  await i18n.loadLanguages(["pt-BR", "en-US"]);
});
afterEach(async () => {
  vi.useRealTimers();
  await harness?.layer.dispose();
  harness = null;
});

const park = {
  "sites.park": (params: { dir: string }) => ({ dir: params.dir, tld: ".test", sites: 1 }),
};

async function failure(promise: Promise<unknown>): Promise<ForgeApiError> {
  try {
    await promise;
  } catch (error) {
    if (isForgeApiError(error)) return error;
    throw error;
  }
  throw new Error("expected a rejection");
}

describe("reconnect queue", () => {
  it("holds a mutating call while reconnecting and sends it after Connected with the same request_id", async () => {
    harness = await startHarness({ scenario: { handlers: park } });
    const h = harness;
    act(() => {
      h.fake.disconnect();
    });
    const intent = newIntent();
    const pending = h.runtime.queue.call("sites.park", { dir: "D:/clientes" }, { intent });
    await act(async () => {
      await Promise.resolve();
    });
    expect(h.runtime.queue.size).toBe(1);
    expect(h.count("sites.park")).toBe(0);
    act(() => {
      h.fake.reconnect();
    });
    await expect(pending).resolves.toEqual({ dir: "D:/clientes", tld: ".test", sites: 1 });
    const sent = h.fake.calls.filter((c) => c.method === "sites.park");
    expect(sent.map((c) => c.requestId)).toEqual([intent.requestId]);
    expect(h.runtime.queue.size).toBe(0);
  });

  it("holds jobs too, keeping their request_id", async () => {
    harness = await startHarness();
    const h = harness;
    act(() => {
      h.fake.disconnect();
    });
    const intent = newIntent();
    const pending = h.runtime.queue.job("tools.install", { specs: ["php@8.4"] }, { intent });
    act(() => {
      h.fake.reconnect();
    });
    const handle = await pending;
    expect(handle.job.request_id).toBe(intent.requestId);
    await expect(handle.done).resolves.toBeNull();
  });

  it("fails with a localized error after 30 s", async () => {
    vi.useFakeTimers();
    harness = await startHarness({ scenario: { handlers: park } });
    const h = harness;
    act(() => {
      h.fake.disconnect();
    });
    const pending = failure(h.runtime.queue.call("sites.park", { dir: "D:/x" }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_HOLD_MS - 1);
    });
    expect(h.runtime.queue.size).toBe(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    const error = await pending;
    expect(RECONNECT_HOLD_MS).toBe(30_000);
    expect([error.code, error.reason, error.method]).toEqual([
      "disconnected",
      "reconnect_timeout",
      "sites.park",
    ]);
    const description = describeError(error);
    const t = (lng: string) => i18n.getFixedT(lng, "errors") as unknown as ErrorTranslate;
    expect(translateError(description, t("pt-BR")).cause).toBe(
      "O daemon não voltou em 30 s, então a ação não foi enviada.",
    );
    expect(translateError(description, t("en-US")).cause).toBe(
      "The daemon did not come back within 30 s, so the action was not sent.",
    );
    // Retrying needs the params the error does not carry: only the daemon start is offered.
    expect(description.actions.map((a) => a.cli)).toEqual(["forge daemon start"]);
    expect(describeError(error, { params: { dir: "D:/x" } }).actions.map((a) => a.cli)).toEqual([
      "forge daemon start",
      "forge park D:/x",
    ]);
    // Never sent, even after the connection comes back.
    act(() => {
      h.fake.reconnect();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });
    expect(h.count("sites.park")).toBe(0);
  });

  it("holds a call that failed with `disconnected` and resends it with the same request_id", async () => {
    harness = await startHarness({ scenario: { handlers: park, latencyMs: 30 } });
    const h = harness;
    const intent = newIntent();
    // Sent while connected; the connection drops before the daemon got it.
    const pending = h.runtime.queue.call("sites.park", { dir: "D:/y" }, { intent });
    act(() => {
      h.fake.disconnect();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 60));
    });
    expect(h.runtime.queue.size).toBe(1);
    expect(h.count("sites.park")).toBe(0);
    act(() => {
      h.fake.reconnect();
    });
    await expect(pending).resolves.toMatchObject({ dir: "D:/y" });
    expect(h.fake.calls.filter((c) => c.method === "sites.park").map((c) => c.requestId)).toEqual([
      intent.requestId,
    ]);
  });

  it("never holds reads, and fails at once when the daemon was stopped on purpose", async () => {
    harness = await startHarness({ scenario: { handlers: park } });
    const h = harness;
    act(() => {
      h.fake.disconnect();
    });
    const read = await failure(h.runtime.queue.call("sites.list", {}));
    expect(read.code).toBe("disconnected");
    expect(h.runtime.queue.size).toBe(0);
    act(() => {
      h.fake.stop("user");
    });
    const stopped = await failure(h.runtime.queue.call("sites.park", { dir: "D:/z" }));
    expect([stopped.code, stopped.reason]).toEqual(["disconnected", "daemon_stopped"]);
  });
});

describe("capabilities", () => {
  const localOnly = Object.entries(METHODS as Record<string, MethodMeta>)
    .filter(([, meta]) => meta.local_only)
    .map(([method]) => method);

  it("marks local_only methods unavailable when hello.local=false", async () => {
    harness = await startHarness({ scenario: { hello: { local: false, role: "admin" } } });
    const h = harness;
    const { result } = renderHook(() => useCapabilities(), {
      wrapper: ({ children }: { children: ReactNode }) => (
        <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
      ),
    });
    expect(localOnly.length).toBeGreaterThan(5);
    for (const method of localOnly) {
      const capability = result.current.can(method);
      if ((result.current.hello?.not_implemented ?? []).includes(method)) continue;
      expect(capability, method).toMatchObject({
        available: false,
        reason: "local_only",
        text: { key: "capability.local_only" },
      });
    }
    expect(result.current.can("sites.list").available).toBe(true);
    // The same daemon over a local connection allows them.
    act(() => {
      h.fake.setHello({ local: true });
    });
    expect(result.current.can("sites.open")).toMatchObject({ available: true, reason: null });
  });

  it("explains role, not implemented, disconnected and unknown methods", () => {
    const hello = {
      session_id: "s",
      api_version: { major: 1, minor: 1 },
      daemon_version: "0.2.0",
      platform: "windows",
      role: "viewer" as const,
      transport: "named_pipe" as const,
      local: true,
      last_event_seq: 0,
      server_time: "2026-10-28T12:00:00Z",
      not_implemented: ["sites.share"],
    };
    expect(capabilityOf("tools.install", hello)).toMatchObject({
      available: false,
      reason: "role",
      text: { key: "capability.role", values: { role: "admin", current: "viewer" } },
    });
    expect(capabilityOf("sites.share", { ...hello, role: "admin" }).reason).toBe("not_implemented");
    expect(capabilityOf("sites.list", null).reason).toBe("disconnected");
    expect(capabilityOf("nope.nope", hello).reason).toBe("unknown_method");
    const table = capabilityTable({ ...hello, role: "admin" });
    expect(Object.keys(table)).toEqual(Object.keys(METHODS));
    for (const capability of Object.values(table)) {
      if (capability.text !== null) {
        expect(i18n.exists(capability.text.key, { lng: "pt-BR", ns: "errors" })).toBe(true);
        expect(i18n.exists(capability.text.key, { lng: "en-US", ns: "errors" })).toBe(true);
      }
    }
  });

  it("useCapability follows the connection", async () => {
    harness = await startHarness();
    const h = harness;
    const { result } = renderHook(() => useCapability("services.start"), {
      wrapper: ({ children }: { children: ReactNode }) => (
        <ForgeStateProvider layer={h.layer}>{children}</ForgeStateProvider>
      ),
    });
    expect(result.current.available).toBe(true);
    act(() => {
      h.fake.disconnect();
    });
    expect(result.current.reason).toBe("disconnected");
  });
});
