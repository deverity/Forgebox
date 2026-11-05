import { describe, expect, it } from "vitest";
import type { ContextStateDto, StreamFrame, SubscribeAnswer } from "./bridge";
import fixture from "./fixtures/bridge-dtos.json";

// The same JSON is checked against the Rust serde shapes in src-tauri/src/bridge/mod.rs:
// typed samples here + a round trip there keep both sides of the bridge in step.
describe("bridge DTOs", () => {
  it("match the shapes src-tauri serializes", () => {
    const state: ContextStateDto = {
      context: "local",
      status: "reconnecting",
      version: 7,
      hello: null,
      generation: null,
      attempt: 2,
      next_retry_ms: 400,
      last_error: {
        code: "disconnected",
        message: "the daemon is not reachable: connection closed",
        details: { reason: "daemon_unreachable", args: {} },
      },
      endpoint: "C:/Forgebox",
      autostart: { initialized: true, stopped_reason: null, next_restart_ms: 59000, restarts: 1 },
    };
    const frame: StreamFrame = {
      stream_id: "01JBRIDGE00000000000000001",
      frame: 3,
      entries: [
        {
          kind: "item",
          item: {
            subscription_id: "01JSUB0000000000000000000A",
            seq: 41,
            event: {
              id: "01JEVENT000000000000000041",
              seq: 41,
              ts: "2026-10-28T12:00:00Z",
              kind: "service.running",
              payload: { v: 1 },
            },
          },
        },
        { kind: "lagged", missed: 5 },
        { kind: "reset" },
        { kind: "end", error: null },
      ],
    };
    const subscribe: SubscribeAnswer = {
      stream_id: "01JBRIDGE00000000000000001",
      method: "events.subscribe",
      resume_point: 40,
      pending: false,
    };
    expect({ state, frame, subscribe }).toEqual(fixture);
  });
});
