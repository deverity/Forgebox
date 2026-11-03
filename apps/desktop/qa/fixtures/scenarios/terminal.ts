// Handlers of the Terminal screen (M2-E3-T15): `terminal.open|list|input|resize|close` over
// fake sessions whose shell echoes what is typed. The stream is fed through
// `FakeBridge.emitTerminal`; Playwright floods it from the page (`QaHandle.bridge`) to measure
// echo latency under a 20 MB output.
import { ulid } from "../../../src/ipc/api";
import type { FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";

const ACK: T.Ack = { ok: true, message: null };
export const QA_PROMPT = "PS C:\\Forgebox> ";

/** Handlers of a fresh set of sessions (state lives in the closure, one set per scenario). */
export function terminalHandlers(): FakeHandlers {
  const sessions = new Map<string, T.TerminalDto>();
  const lastInput = new Map<string, number>();
  return {
    "terminal.open": (params, { fake }) => {
      const id = ulid();
      const dto: T.TerminalDto = {
        id,
        title: params.title ?? (params.site ? `Terminal em ${params.site}` : "Terminal"),
        shell: params.shell ?? "pwsh",
        cwd: params.dir ?? "C:/Forgebox",
        site_id: null,
        cols: params.cols ?? 120,
        rows: params.rows ?? 30,
        pid: 4000 + sessions.size,
        started_at: new Date().toISOString(),
        last_seq: 0,
      };
      sessions.set(id, dto);
      fake.emitTerminal(id, { kind: "output", data: QA_PROMPT });
      return { ...dto, last_seq: fake.lastSeq(`terminal:${id}`) };
    },
    "terminal.list": (_params, { fake }) => ({
      terminals: [...sessions.values()].map((dto) => ({
        ...dto,
        last_seq: fake.lastSeq(`terminal:${dto.id}`),
      })),
    }),
    "terminal.input": (params, { fake }) => {
      const key = `${params.id}:${params.writer}`;
      if (params.input_seq <= (lastInput.get(key) ?? 0)) return ACK;
      lastInput.set(key, params.input_seq);
      if (params.data === "exit\r") {
        fake.emitTerminal(params.id, { kind: "output", data: "exit\r\n" });
        fake.emitTerminal(params.id, { kind: "exited", exit_code: 3 });
        const dto = sessions.get(params.id);
        if (dto)
          sessions.set(params.id, { ...dto, exit_code: 3, exited_at: new Date().toISOString() });
        return ACK;
      }
      // The shell echoes what was typed (Enter starts a new prompt line).
      const echo = params.data.replaceAll("\r", `\r\n${QA_PROMPT}`);
      fake.emitTerminal(params.id, { kind: "output", data: echo });
      return ACK;
    },
    "terminal.resize": (params) => {
      const dto = sessions.get(params.id);
      if (dto) sessions.set(params.id, { ...dto, cols: params.cols, rows: params.rows });
      return ACK;
    },
    "terminal.close": (params) => {
      sessions.delete(params.id);
      return ACK;
    },
  };
}
