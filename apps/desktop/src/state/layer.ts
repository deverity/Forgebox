// The state layer of the app: one `ContextRuntime` per context (created on first use, each
// keeping its own stores and cache, docs/13 §5), sharing one TanStack `QueryClient` whose
// keys start with the context id.
import { QueryClient } from "@tanstack/react-query";
import type { Bridge, ContextId } from "../ipc/bridge";
import { ContextRuntime, type RuntimeOptions } from "./runtime";

export class StateLayer {
  readonly bridge: Bridge;
  readonly queryClient: QueryClient;
  private readonly options: RuntimeOptions;
  private readonly runtimes = new Map<ContextId, ContextRuntime>();

  constructor(bridge: Bridge, options: RuntimeOptions = {}) {
    this.bridge = bridge;
    this.queryClient =
      options.queryClient ??
      new QueryClient({
        defaultOptions: {
          // Events invalidate what changed; refetching on focus would only add load.
          queries: { staleTime: 30_000, retry: 1, refetchOnWindowFocus: false },
        },
      });
    this.options = { ...options, queryClient: this.queryClient };
  }

  /** The runtime of `context` (created, not started, on first use). */
  runtime(context: ContextId): ContextRuntime {
    let runtime = this.runtimes.get(context);
    if (runtime === undefined) {
      runtime = new ContextRuntime(this.bridge, context, this.options);
      this.runtimes.set(context, runtime);
    }
    return runtime;
  }

  async dispose(): Promise<void> {
    const runtimes = [...this.runtimes.values()];
    this.runtimes.clear();
    await Promise.all(runtimes.map((runtime) => runtime.dispose()));
    this.queryClient.clear();
  }
}
