//! `events.subscribe|unsubscribe|list`: the event stream with `resume_from` (ADR-0009,
//! rule 3).
//!
//! Resuming after `seq` N delivers every matching event with `seq > N` exactly once, in
//! order: events older than the bus's in-memory ring are read from the `events` table
//! (when a store is configured), then the ring, then live events; the ring snapshot and
//! the live subscription are taken atomically ([`EventBus::subscribe_from`]). Holes
//! (pruned rows, more than `replay_limit` events behind, no store) are reported with
//! `events.lagged` at the place they occur. Live events use drop-oldest per subscriber.

use std::ops::RangeInclusive;
use std::time::Duration;

use forge_core::error::Result;
use forge_core::event::{EventBus, Resume, StreamItem};
use forge_core::store::{EventRepo, StoreReader};

use super::outbox::StreamSender;
use super::router::Ctx;
use crate::api::{
    EventDto, EventEnvelope, EventFilter, EventPageDto, EventsListParams, EventsSubscribeParams,
    SubscriptionDto,
};

/// How long a replayed item may wait for room in the subscriber's queue.
const REPLAY_WAIT: Duration = Duration::from_secs(2);
/// Rows read per store query while replaying.
const STORE_PAGE: usize = 1000;

/// Where events come from: the bus, and optionally the store for older history.
#[derive(Clone, Debug)]
pub struct EventSource {
    bus: EventBus,
    store: Option<StoreReader>,
}

impl EventSource {
    pub fn new(bus: EventBus) -> Self {
        Self { bus, store: None }
    }

    /// Reads events older than the in-memory ring from the `events` table.
    pub fn with_store(mut self, reader: StoreReader) -> Self {
        self.store = Some(reader);
        self
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }
}

fn envelope(sender: &StreamSender, event: forge_core::event::Event) -> EventEnvelope {
    EventEnvelope {
        subscription_id: sender.id(),
        seq: event.seq,
        event: EventDto::from(event),
    }
}

pub(crate) async fn subscribe(
    source: EventSource,
    replay_limit: u64,
    params: EventsSubscribeParams,
    ctx: Ctx,
) -> Result<SubscriptionDto> {
    let sender = ctx.open_stream(None)?;
    let resume = match params.resume_from {
        Some(after) => source.bus.subscribe_from(after),
        None => {
            let stream = source.bus.subscribe();
            Resume {
                replay: Vec::new(),
                stream,
                gap: None,
                last_seq: source.bus.last_seq(),
            }
        }
    };
    let last_seq = resume.last_seq;
    tokio::spawn(run_stream(
        source,
        replay_limit,
        params.filter,
        resume,
        sender.clone(),
    ));
    Ok(SubscriptionDto {
        subscription_id: sender.id(),
        stream: "events".into(),
        last_seq,
    })
}

async fn run_stream(
    source: EventSource,
    replay_limit: u64,
    filter: EventFilter,
    resume: Resume,
    sender: StreamSender,
) {
    let Resume {
        replay,
        mut stream,
        gap,
        ..
    } = resume;
    if let Some(gap) = gap {
        backfill(&source, replay_limit, &filter, gap, &sender).await;
    }
    for event in replay {
        if sender.is_closed() {
            return;
        }
        if filter.matches_event(&event) {
            sender
                .send_wait(&envelope(&sender, event), REPLAY_WAIT)
                .await;
        }
    }
    loop {
        tokio::select! {
            biased;
            () = sender.closed() => return,
            item = stream.recv_item() => match item {
                Some(StreamItem::Event(event)) => {
                    if filter.matches_event(&event) && !sender.send(&envelope(&sender, event)) {
                        return;
                    }
                }
                Some(StreamItem::Lagged { missed }) => sender.lagged(missed),
                None => {
                    // The bus is gone (daemon shutting down).
                    sender.close();
                    return;
                }
            },
        }
    }
}

/// Sends the events of `gap` from the store, reporting holes as lag.
async fn backfill(
    source: &EventSource,
    replay_limit: u64,
    filter: &EventFilter,
    gap: RangeInclusive<u64>,
    sender: &StreamSender,
) {
    let (first, last) = (*gap.start(), *gap.end());
    let wanted = last - first + 1;
    let Some(reader) = &source.store else {
        sender.lagged(wanted);
        return;
    };
    // Cursor: the last seq accounted for (sent, filtered out or reported missing).
    let mut cursor = first - 1;
    if wanted > replay_limit {
        sender.lagged(wanted - replay_limit);
        cursor = last - replay_limit;
    }
    while cursor < last && !sender.is_closed() {
        let after = cursor;
        let page = reader
            .query(move |conn| EventRepo::new(conn).list_since(after, STORE_PAGE))
            .await;
        let events = match page {
            Ok(events) => events,
            Err(err) => {
                tracing::warn!(%err, "event replay from the store failed");
                break;
            }
        };
        if events.is_empty() {
            break;
        }
        for event in events {
            if event.seq > last {
                cursor = last;
                break;
            }
            if event.seq > cursor + 1 {
                sender.lagged(event.seq - cursor - 1);
            }
            cursor = event.seq;
            if filter.matches_event(&event) {
                sender
                    .send_wait(&envelope(sender, event), REPLAY_WAIT)
                    .await;
            }
        }
    }
    if cursor < last {
        sender.lagged(last - cursor);
    }
}

pub(crate) async fn list(source: EventSource, params: EventsListParams) -> Result<EventPageDto> {
    let limit = params.limit.unwrap_or(500).clamp(1, 5000) as usize;
    let after = params.after_seq;
    let last_seq = source.bus.last_seq();
    let in_memory = source
        .bus
        .oldest_in_history()
        .is_some_and(|oldest| after.saturating_add(1) >= oldest);
    let scanned = match (&source.store, in_memory) {
        (Some(reader), false) => {
            reader
                .query(move |conn| EventRepo::new(conn).list_since(after, limit))
                .await?
        }
        _ => source.bus.history_since(after, limit),
    };
    let next_after = scanned.last().map_or(after, |e| e.seq);
    let has_more = next_after < last_seq;
    let events = scanned
        .into_iter()
        .filter(|e| params.filter.matches_event(e))
        .map(EventDto::from)
        .collect();
    Ok(EventPageDto {
        events,
        next_after,
        last_seq,
        has_more,
    })
}
