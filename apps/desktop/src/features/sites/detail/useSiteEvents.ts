import { useEffect, useRef } from "react";
import type * as T from "../../../ipc/generated";
import { useForgeApi } from "../../../state";

const KINDS = ["service.*", "site.php_changed"];

/**
 * Follows the events of one site: `site.php_changed` and `service.*` (the payload carries
 * `site_id` for site processes). One `events.subscribe` per mounted screen, filtered by kind;
 * the site match is done here because the payload is what carries it. The state layer's own
 * stream keeps the stores current; this one drives the screen's transient state (a switch in
 * flight, the rows of Processos).
 */
export function useSiteEvents(siteId: string, onEvent: (event: T.EventDto) => void): void {
  const api = useForgeApi();
  const handler = useRef(onEvent);
  useEffect(() => {
    handler.current = onEvent;
  }, [onEvent]);
  useEffect(() => {
    const run = { alive: true, unsubscribe: null as (() => Promise<void>) | null };
    api
      .subscribe("events.subscribe", { filter: { kinds: KINDS }, resume_from: null }, (entries) => {
        for (const entry of entries) {
          if (entry.kind !== "item") continue;
          const event = entry.item.event;
          const payload = event.payload as { site_id?: unknown } | null;
          if (payload?.site_id === siteId) handler.current(event);
        }
      })
      .then(
        (handle) => {
          if (run.alive) run.unsubscribe = () => handle.unsubscribe();
          else void handle.unsubscribe().catch(() => undefined);
        },
        () => {
          // Best effort: the sites.ps refetch and the job result still settle the screen.
        },
      );
    return () => {
      run.alive = false;
      void run.unsubscribe?.().catch(() => undefined);
    };
  }, [api, siteId]);
}
