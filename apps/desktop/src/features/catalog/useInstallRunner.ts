import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
import { startJobAction } from "../../actions/registry";
import { installTools } from "../../actions/tools";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import { useRuntime } from "../../state";
import type { InstallProgressItem } from "../shared/install-progress";

export interface InstallRequest {
  specs: readonly string[];
  variant?: string | null;
  /** What the progress row calls it ("PHP 8.4.27"). */
  label: string;
}

export interface InstallRunner {
  items: readonly InstallProgressItem[];
  /** The failure to START an install (a failed job shows in its own row). */
  failure: ForgeApiError | null;
  starting: boolean;
  start: (request: InstallRequest) => void;
  dismiss: (item: InstallProgressItem) => void;
  /** Starts the same install again (after a failure or a cancel). */
  retry: (item: InstallProgressItem) => void;
}

/**
 * Starts `tools.install` jobs through the registered action and keeps the rows the
 * <InstallProgressList> follows. When a job ends the cached catalog data is refreshed
 * (releases and `installed` flags), besides the events that already invalidate `tools.*`.
 */
export function useInstallRunner(): InstallRunner {
  const runtime = useRuntime();
  const client = useQueryClient();
  const [items, setItems] = useState<readonly InstallProgressItem[]>([]);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);
  const [starting, setStarting] = useState(false);
  const counter = useRef(0);
  const requests = useRef(new Map<string, InstallRequest>());

  const start = useCallback(
    (request: InstallRequest) => {
      setStarting(true);
      setFailure(null);
      startJobAction(
        runtime.queue,
        installTools,
        { specs: request.specs, variant: request.variant ?? null },
        { intent: newIntent() },
      ).then(
        (handle) => {
          setStarting(false);
          counter.current += 1;
          const key = `${handle.job.id}-${String(counter.current)}`;
          requests.current.set(key, request);
          setItems((current) => [...current, { key, jobId: handle.job.id, label: request.label }]);
          const refresh = () => {
            void client.invalidateQueries({
              queryKey: [runtime.api.context],
              predicate: (query) => String(query.queryKey[1]).startsWith("tools."),
            });
          };
          // The job's own outcome is shown by its row; here it only refreshes the data.
          handle.done.then(refresh, refresh);
        },
        (error: unknown) => {
          setStarting(false);
          setFailure(toForgeApiError(error, { method: installTools.method }));
        },
      );
    },
    [runtime, client],
  );

  const dismiss = useCallback((item: InstallProgressItem) => {
    setItems((current) => current.filter((candidate) => candidate.key !== item.key));
  }, []);

  const retry = useCallback(
    (item: InstallProgressItem) => {
      const request = requests.current.get(item.key);
      dismiss(item);
      if (request !== undefined) start(request);
    },
    [dismiss, start],
  );

  return { items, failure, starting, start, dismiss, retry };
}
