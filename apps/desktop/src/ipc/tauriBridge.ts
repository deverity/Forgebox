// The app's Bridge: generic Tauri commands of src-tauri/src/bridge/commands.rs (snake_case
// arguments, like every DTO) and the `forge://connection` event. Frames arrive over a Tauri
// Channel and each one is acknowledged (`ipc_ack`) after the handler returned, which is the
// flow control of the stream pump: a stalled page stops receiving instead of piling up
// (ADR-0034).
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  CONNECTION_EVENT,
  type Bridge,
  type ContextStateDto,
  type StreamFrame,
  type StreamHandle,
  type SubscribeAnswer,
} from "./bridge";
import { toForgeApiError, type ErrorOrigin } from "./errors";

/** Names of the commands (src-tauri `generate_handler!`). */
export const COMMANDS = {
  state: "ipc_state",
  call: "ipc_call",
  subscribe: "ipc_subscribe",
  ack: "ipc_ack",
  unsubscribe: "ipc_unsubscribe",
  start: "ipc_start",
} as const;

async function command<T>(
  name: string,
  args: Record<string, unknown>,
  origin: ErrorOrigin = {},
): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw toForgeApiError(error, origin);
  }
}

export function createTauriBridge(): Bridge {
  return {
    state: (context) => command<ContextStateDto>(COMMANDS.state, { context }),

    call: (request) =>
      command<unknown>(
        COMMANDS.call,
        {
          context: request.context,
          method: request.method,
          params: request.params ?? null,
          request_id: request.request_id,
          timeout_ms: request.timeout_ms ?? null,
        },
        { method: request.method, requestId: request.request_id },
      ),

    async subscribe(context, method, params, onFrame) {
      const channel = new Channel<StreamFrame>();
      channel.onmessage = (frame) => {
        try {
          onFrame(frame);
        } catch (error) {
          console.error(`stream ${method}: frame handler failed`, error);
        } finally {
          invoke(COMMANDS.ack, { stream_id: frame.stream_id, frame: frame.frame }).catch(() => {
            // The stream is gone; nothing to acknowledge.
          });
        }
      };
      const answer = await command<SubscribeAnswer>(
        COMMANDS.subscribe,
        { context, method, params: params ?? null, on_frame: channel },
        { method },
      );
      const handle: StreamHandle = {
        id: answer.stream_id,
        method: answer.method,
        resumePoint: answer.resume_point,
        pending: answer.pending,
        async unsubscribe() {
          try {
            await command<null>(
              COMMANDS.unsubscribe,
              { context, stream_id: answer.stream_id },
              { method },
            );
          } catch (error) {
            // Already ended (`not_found`): unsubscribing twice is fine.
            if (toForgeApiError(error).code !== "not_found") throw error;
          }
        },
      };
      return handle;
    },

    start: (context, options) =>
      command<ContextStateDto>(COMMANDS.start, {
        context,
        initialize: options?.initialize ?? false,
      }),

    onConnection: (listener) =>
      listen<ContextStateDto>(CONNECTION_EVENT, (event) => {
        listener(event.payload);
      }),
  };
}
