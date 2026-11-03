// Small hooks of the catalog screens: debounce, visibility (lazy icons) and job-backed queries.
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import type { ParamsOf } from "../../ipc/api";
import type { ForgeApiError } from "../../ipc/errors";
import type { ApiMethods, MethodName } from "../../ipc/methods";
import { useCapability, useForgeApi } from "../../state";

/** `value` after it stayed unchanged for `delayMs`. */
export function useDebounced<V>(value: V, delayMs: number): V {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => {
      setSettled(value);
    }, delayMs);
    return () => {
      clearTimeout(timer);
    };
  }, [value, delayMs]);
  return settled;
}

/**
 * True once the element has entered the viewport (IntersectionObserver); it stays true, so a
 * card that scrolled away keeps its icon. Without IntersectionObserver (old webview, jsdom)
 * everything counts as visible.
 */
export function useVisibleOnce<E extends Element>(): [React.RefObject<E | null>, boolean] {
  const ref = useRef<E>(null);
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const element = ref.current;
    if (visible || element === null) return undefined;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setVisible(true);
        observer.disconnect();
      }
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, [visible]);
  return [ref, visible];
}

type CatalogJob = Extract<MethodName, "tools.releases" | "tools.plan">;

type JobResult<M extends CatalogJob> = ApiMethods[M] extends { job_result: infer R } ? R : never;

/**
 * A non-mutating job (`tools.releases`, `tools.plan`) as a cached query: the result of the
 * job, refetched on demand. Disabled while the daemon does not offer the method (`not_implemented`
 * shows in the capability, the screen then explains it instead of calling).
 */
export function useJobQuery<M extends CatalogJob>(
  method: M,
  params: ParamsOf<M>,
  options: { enabled?: boolean } = {},
): UseQueryResult<JobResult<M>, ForgeApiError> {
  const api = useForgeApi();
  const capability = useCapability(method);
  return useQuery<JobResult<M>, ForgeApiError>({
    // Same shape as `queryKeys.call` ([context, method, params]) so context-wide invalidation applies.
    queryKey: [api.context, method, params] as const,
    queryFn: async () => {
      const handle = await api.job(method, params);
      return await handle.done;
    },
    enabled: (options.enabled ?? true) && capability.available,
    // Releases come from the providers: the daemon caches them; never refetch on focus.
    staleTime: 60_000,
    retry: false,
  });
}
