// The hooks behind the Atenção sources (one per source, registered by `slots.ts`). Each reads
// what the state layer already keeps current and hands it to the pure functions of
// `attention.ts`; the queries are invalidated by the events that make them stale.
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import type { AttentionItem } from "../../app/slots";
import type { EventDto } from "../../ipc/generated";
import {
  queryKeys,
  useAttention,
  useForgeApi,
  useForgeQuery,
  useInstances,
  useSystem,
  useToolUpdates,
  useTools,
} from "../../state";
import { useNow } from "../services/useNow";
import {
  caItems,
  certItems,
  dnsItems,
  driftItems,
  driftedFiles,
  failedItems,
  pendingUpdates,
  portItems,
  updateItems,
} from "./attention";

const NO_ITEMS: readonly AttentionItem[] = [];
const DRIFT_KINDS = ["config.drift_detected", "config.rendered"];
const PAGE_LIMIT = 5000;
/** The `now` of the time-based sources moves once a minute: coarse on purpose. */
const CLOCK_MS = 60_000;

export function usePortItems(): readonly AttentionItem[] {
  const conflicts = useAttention((state) => state.portConflicts);
  const ports = useForgeQuery("services.ports", {});
  const external = ports.data?.external;
  return useMemo(() => portItems(conflicts, external ?? []), [conflicts, external]);
}

export function useUpdateItems(): readonly AttentionItem[] {
  const { updates } = useToolUpdates();
  const tools = useTools((state) => state.tools);
  return useMemo(
    () => updateItems(pendingUpdates(updates, Object.values(tools))),
    [updates, tools],
  );
}

export function useDriftItems(): readonly AttentionItem[] {
  const api = useForgeApi();
  const live = useAttention((state) => state.configDrift);
  const now = useNow(CLOCK_MS);
  const params = { after_seq: 0, limit: PAGE_LIMIT, filter: { kinds: DRIFT_KINDS } };
  // `config.rendered|drift_detected` invalidate `events.list` (queries.ts), so this refetches.
  const history = useQuery({
    queryKey: queryKeys.call(api.context, "events.list", params),
    queryFn: async () => {
      let events: EventDto[] = [];
      let after = 0;
      for (;;) {
        const page = await api.call("events.list", { ...params, after_seq: after });
        events = [...events, ...page.events].slice(-PAGE_LIMIT);
        if (!page.has_more || page.next_after <= after) return events;
        after = page.next_after;
      }
    },
  });
  const events = history.data;
  return useMemo(() => driftItems(driftedFiles(events ?? [], live, now)), [events, live, now]);
}

export function useCaItems(): readonly AttentionItem[] {
  const ca = useSystem((state) => state.ca);
  const now = useNow(CLOCK_MS);
  return useMemo(() => caItems(ca, now), [ca, now]);
}

export function useCertItems(): readonly AttentionItem[] {
  const list = useForgeQuery("tls.list", {});
  const now = useNow(CLOCK_MS);
  const certificates = list.data?.certificates;
  return useMemo(
    () => (certificates === undefined ? NO_ITEMS : certItems(certificates, now)),
    [certificates, now],
  );
}

export function useDnsItems(): readonly AttentionItem[] {
  const dns = useSystem((state) => state.dns);
  return useMemo(() => dnsItems(dns), [dns]);
}

export function useFailedItems(): readonly AttentionItem[] {
  const instances = useInstances();
  return useMemo(() => failedItems(Object.values(instances)), [instances]);
}
