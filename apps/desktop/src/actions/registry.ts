// Action registry (docs/07 §1.7, M2-E1-T13): every mutating API method the GUI calls is a
// registered action, `defineAction({id, method, toParams})`. The action is the single place
// that knows how the screen's input becomes the method's params, so the same `ParamsOf<M>`
// feeds the call, the equivalent `forge` command shown to the user (`renderCli`) and the
// E4-T5 parity gate (`ACTIONS` lists them all). The ESLint rule `forgebox/no-direct-mutation`
// forbids calling a mutating method with `api.call`/`api.job`/`queue.call` outside this folder.
import type { CallOptions, JobOptions, ParamsOf } from "../ipc/api";
import { renderCli } from "../ipc/cli";
import { METHODS, type MethodName } from "../ipc/methods";
import type { ReconnectQueue } from "../state/reconnectQueue";

export interface ActionDef<M extends MethodName, I> {
  /** Stable id (`services.start`, `sites.create`...); unique across the registry. */
  id: string;
  method: M;
  /** Screen input to the method's params. */
  toParams: (input: I) => ParamsOf<M>;
}

export interface Action<M extends MethodName = MethodName, I = unknown> extends ActionDef<M, I> {
  readonly kind: "call" | "job";
  /** The `forge` command equivalent to running the action with `input` (null: none). */
  cli(input: I): string | null;
}

const registry = new Map<string, Action<MethodName, never>>();

/** Registers an action. Streams cannot be actions; ids are unique. */
export function defineAction<M extends MethodName, I = void>(def: ActionDef<M, I>): Action<M, I> {
  const kind = METHODS[def.method].kind;
  if (kind === "stream") throw new Error(`action ${def.id}: ${def.method} is a stream`);
  if (registry.has(def.id)) throw new Error(`action ${def.id} is already defined`);
  const action: Action<M, I> = {
    ...def,
    kind,
    cli: (input) => renderCli(def.method, def.toParams(input)),
  };
  registry.set(def.id, action);
  return action;
}

/** Every registered action (parity gate E4-T5). */
export function listActions(): readonly Action<MethodName, never>[] {
  return [...registry.values()];
}

export function getAction(id: string): Action<MethodName, never> | undefined {
  return registry.get(id);
}

/** Test helper: forget the actions defined by a test. */
export function resetActionsForTest(): void {
  registry.clear();
}

/** Options of a run; a job action also reports `onProgress`. */
export type RunOptions = JobOptions;

/**
 * Runs `action` through the context's reconnect queue (a mutating call waits for a daemon
 * that is coming back). A job action resolves when the job ends, with its result; a failed
 * job rejects with its `ForgeApiError`.
 */
export async function runAction<M extends MethodName, I>(
  queue: ReconnectQueue,
  action: Action<M, I>,
  input: I,
  options: RunOptions = {},
): Promise<unknown> {
  const params = action.toParams(input);
  // The method is a variable here: the typed overloads are for literal names, the registry
  // already tied `params` to `ParamsOf<M>`.
  if (action.kind === "job") {
    const handle = await (
      queue.job as (m: string, p: unknown, o: JobOptions) => Promise<{ done: Promise<unknown> }>
    )(action.method, params, options);
    return handle.done;
  }
  return (queue.call as (m: string, p: unknown, o: CallOptions) => Promise<unknown>)(
    action.method,
    params,
    options,
  );
}
