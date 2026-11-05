// The Atividade feed: the last 50 humanizable events, kept live. The history comes from
// `events.list` (oldest to newest pages, filtered to the kinds worth showing); new events come
// from the runtime's raw feed. Events are deduplicated by id, newest first.
import { useEffect, useState } from "react";
import type { EventDto } from "../../ipc/generated";
import { useForgeApi, useRuntime } from "../../state";
import { ACTIVITY_KINDS, humanizeEvent } from "../shared/activity/humanizeEvent";
import { ACTIVITY_LIMIT } from "./model";

const PAGE_LIMIT = 5000;

/** Merges `incoming` into `current`: humanizable only, unique by id, newest first, capped. */
export function mergeActivity(
  current: readonly EventDto[],
  incoming: readonly EventDto[],
  limit = ACTIVITY_LIMIT,
): EventDto[] {
  const byId = new Map<string, EventDto>();
  for (const event of [...current, ...incoming]) {
    if (humanizeEvent(event) !== null) byId.set(event.id, event);
  }
  return [...byId.values()].sort((a, b) => b.seq - a.seq).slice(0, limit);
}

export interface Activity {
  events: readonly EventDto[];
  loading: boolean;
}

export function useActivity(enabled = true): Activity {
  const runtime = useRuntime();
  const api = useForgeApi();
  const [events, setEvents] = useState<readonly EventDto[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!enabled) return undefined;
    const session = { alive: true };
    const live: EventDto[] = [];
    let ready = false;
    const off = runtime.onEvents((batch) => {
      if (!session.alive) return;
      if (ready) setEvents((current) => mergeActivity(current, batch));
      else live.push(...batch);
    });
    void (async () => {
      let tail: EventDto[] = [];
      try {
        let after = 0;
        for (;;) {
          const page = await api.call("events.list", {
            after_seq: after,
            limit: PAGE_LIMIT,
            filter: { kinds: [...ACTIVITY_KINDS] },
          });
          tail = mergeActivity(tail, page.events);
          if (!page.has_more || page.next_after <= after) break;
          after = page.next_after;
        }
      } catch {
        // No history (daemon away): the live feed still works.
      }
      if (!session.alive) return;
      ready = true;
      setEvents(mergeActivity(tail, live));
      setLoading(false);
    })();
    return () => {
      session.alive = false;
      off();
    };
  }, [runtime, api, enabled]);

  return { events, loading };
}
