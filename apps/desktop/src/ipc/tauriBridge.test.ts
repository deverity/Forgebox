import { beforeEach, describe, expect, it, vi } from "vitest";
import { CONNECTION_EVENT, LOCAL_CONTEXT, type ContextStateDto, type StreamFrame } from "./bridge";
import { ForgeApiError } from "./errors";
import { COMMANDS, createTauriBridge } from "./tauriBridge";

const tauri = vi.hoisted(() => {
  // Stands in for `Channel<StreamFrame>`: the bridge only sets `onmessage`.
  class FakeChannel {
    onmessage: (message: unknown) => void = () => undefined;
  }
  return {
    invoke: vi.fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>(),
    listen:
      vi.fn<
        (event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>
      >(),
    Channel: FakeChannel,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke, Channel: tauri.Channel }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

beforeEach(() => {
  tauri.invoke.mockReset();
  tauri.listen.mockReset();
});

describe("tauriBridge", () => {
  it("sends calls to ipc_call with snake_case arguments", async () => {
    tauri.invoke.mockResolvedValue({ server_time: "now" });
    const bridge = createTauriBridge();
    const answer = await bridge.call({
      context: LOCAL_CONTEXT,
      method: "system.ping",
      params: {},
      request_id: "01J0000000000000000000000A",
    });
    expect(answer).toEqual({ server_time: "now" });
    expect(tauri.invoke).toHaveBeenCalledWith(COMMANDS.call, {
      context: "local",
      method: "system.ping",
      params: {},
      request_id: "01J0000000000000000000000A",
      timeout_ms: null,
    });
  });

  it("turns the command's ApiError into a ForgeApiError with its origin", async () => {
    tauri.invoke.mockRejectedValue({
      code: "disconnected",
      message: "the daemon is not reachable",
      details: { reason: "daemon_unreachable", args: {} },
    });
    const bridge = createTauriBridge();
    const error: unknown = await bridge
      .call({
        context: LOCAL_CONTEXT,
        method: "sites.list",
        request_id: "01J0000000000000000000000B",
      })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ForgeApiError);
    const api = error as ForgeApiError;
    expect([api.code, api.reason, api.method, api.requestId]).toEqual([
      "disconnected",
      "daemon_unreachable",
      "sites.list",
      "01J0000000000000000000000B",
    ]);
  });

  it("pumps frames into the handler and acknowledges each one after it ran", async () => {
    let channel: { onmessage: (frame: StreamFrame) => void } | undefined;
    tauri.invoke.mockImplementation((command, args) => {
      if (command === COMMANDS.subscribe) {
        channel = args?.on_frame as typeof channel;
        return Promise.resolve({
          stream_id: "S1",
          method: "events.subscribe",
          resume_point: 7,
          pending: false,
        });
      }
      return Promise.resolve(null);
    });
    const bridge = createTauriBridge();
    const order: string[] = [];
    const handle = await bridge.subscribe(
      LOCAL_CONTEXT,
      "events.subscribe",
      { filter: {} },
      (frame) => {
        order.push(`frame ${String(frame.frame)}`);
      },
    );
    expect([handle.id, handle.resumePoint, handle.pending]).toEqual(["S1", 7, false]);
    expect(tauri.invoke).toHaveBeenCalledWith(COMMANDS.subscribe, {
      context: "local",
      method: "events.subscribe",
      params: { filter: {} },
      on_frame: channel,
    });
    tauri.invoke.mockImplementation((command, args) => {
      order.push(`${command} ${String(args?.frame)}`);
      return Promise.resolve(null);
    });
    channel?.onmessage({ stream_id: "S1", frame: 1, entries: [{ kind: "lagged", missed: 3 }] });
    expect(order).toEqual(["frame 1", `${COMMANDS.ack} 1`]);
  });

  it("acknowledges a frame even when the handler throws (the pump must not stall)", async () => {
    let channel: { onmessage: (frame: StreamFrame) => void } | undefined;
    tauri.invoke.mockImplementation((command, args) => {
      if (command === COMMANDS.subscribe) {
        channel = args?.on_frame as typeof channel;
        return Promise.resolve({
          stream_id: "S3",
          method: "logs.subscribe",
          resume_point: 0,
          pending: false,
        });
      }
      return Promise.resolve(null);
    });
    const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const bridge = createTauriBridge();
    await bridge.subscribe(LOCAL_CONTEXT, "logs.subscribe", { target: "daemon" }, () => {
      throw new Error("boom");
    });
    channel?.onmessage({ stream_id: "S3", frame: 4, entries: [] });
    expect(tauri.invoke).toHaveBeenLastCalledWith(COMMANDS.ack, { stream_id: "S3", frame: 4 });
    expect(errors).toHaveBeenCalledOnce();
    errors.mockRestore();
  });

  it("unsubscribes once and ignores a stream that already ended", async () => {
    tauri.invoke.mockImplementation((command) => {
      if (command === COMMANDS.subscribe) {
        return Promise.resolve({
          stream_id: "S2",
          method: "events.subscribe",
          resume_point: null,
          pending: true,
        });
      }
      if (command === COMMANDS.unsubscribe) {
        return Promise.reject(new Error("unused"));
      }
      return Promise.resolve(null);
    });
    const bridge = createTauriBridge();
    const handle = await bridge.subscribe(LOCAL_CONTEXT, "events.subscribe", {}, () => undefined);
    expect(handle.pending).toBe(true);
    tauri.invoke.mockRejectedValueOnce({
      code: "not_found",
      message: "no stream",
      details: { reason: "unknown_stream", args: {} },
    });
    await expect(handle.unsubscribe()).resolves.toBeUndefined();
    expect(tauri.invoke).toHaveBeenLastCalledWith(COMMANDS.unsubscribe, {
      context: "local",
      stream_id: "S2",
    });
  });

  it("reads the state, starts the daemon and listens to forge://connection", async () => {
    const state = { context: "local", status: "connected", version: 3 } as ContextStateDto;
    tauri.invoke.mockResolvedValue(state);
    const bridge = createTauriBridge();
    await expect(bridge.state(LOCAL_CONTEXT)).resolves.toBe(state);
    expect(tauri.invoke).toHaveBeenLastCalledWith(COMMANDS.state, { context: "local" });
    await bridge.start(LOCAL_CONTEXT, { initialize: true });
    expect(tauri.invoke).toHaveBeenLastCalledWith(COMMANDS.start, {
      context: "local",
      initialize: true,
    });

    const unlisten = vi.fn();
    let handler: ((event: { payload: unknown }) => void) | undefined;
    tauri.listen.mockImplementation((_event, h) => {
      handler = h;
      return Promise.resolve(unlisten);
    });
    const seen: ContextStateDto[] = [];
    const stop = await bridge.onConnection((s) => seen.push(s));
    expect(tauri.listen).toHaveBeenCalledWith(CONNECTION_EVENT, expect.any(Function));
    handler?.({ payload: state });
    expect(seen).toEqual([state]);
    stop();
    expect(unlisten).toHaveBeenCalledOnce();
  });
});
