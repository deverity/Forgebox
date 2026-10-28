//! Persistence of the event stream (docs/01 §6, docs/03 §8): every event published on
//! the bus goes to the `events` table, so `events.subscribe {resume_from}` can replay what
//! the in-memory ring no longer has (ADR-0015) and `seq` continues after a restart (the
//! bus is seeded with the largest stored `seq`).
//!
//! One task subscribes before anything else publishes, batches what is queued (one writer
//! operation per batch, at most [`BATCH`] events) and keeps the table at
//! [`KEEP_EVENTS`] rows. A slow writer makes it lag behind the broadcast channel; the
//! missed events are taken back from the bus ring (`history_since`), so nothing is lost
//! unless the ring itself overflowed. On stop it drains what is queued and returns once
//! written.

use std::time::Duration;

use forge_core::event::{Event, EventBus, EventStream, StreamItem};
use forge_core::store::{EventRepo, StoreWriter};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Events written per writer operation.
pub const BATCH: usize = 256;
/// Rows kept in `events` (ring of docs/01 §6).
pub const KEEP_EVENTS: u64 = 50_000;
/// Prune once every this many inserted events.
const PRUNE_EVERY: u64 = 1_000;
/// Budget of one batch write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Handle of the persistence task.
#[derive(Debug)]
pub struct EventLog {
    stop: CancellationToken,
    task: JoinHandle<u64>,
}

impl EventLog {
    /// Subscribes to `bus` now and starts writing through `writer`.
    pub fn spawn(bus: &EventBus, writer: StoreWriter) -> Self {
        // Subscription and resume point taken atomically: every event after `last_seq`
        // is either in `replay` or in the stream.
        let resume = bus.subscribe_from(bus.last_seq());
        let stop = CancellationToken::new();
        let task = tokio::spawn(run(
            bus.clone(),
            resume.stream,
            resume.replay,
            resume.last_seq,
            writer,
            stop.clone(),
        ));
        Self { stop, task }
    }

    /// Writes what is still queued and stops; returns how many events were persisted
    /// since the start.
    pub async fn stop(self, timeout: Duration) -> u64 {
        self.stop.cancel();
        match tokio::time::timeout(timeout, self.task).await {
            Ok(Ok(written)) => written,
            Ok(Err(e)) => {
                tracing::error!(%e, "event log task failed");
                0
            }
            Err(_) => {
                tracing::warn!(?timeout, "event log did not finish in time");
                0
            }
        }
    }
}

async fn write(writer: &StoreWriter, batch: Vec<Event>) -> forge_core::Result<()> {
    writer
        .execute_timeout(WRITE_TIMEOUT, move |conn| {
            let repo = EventRepo::new(conn);
            for event in &batch {
                repo.insert(event)?;
            }
            Ok(())
        })
        .await
}

async fn run(
    bus: EventBus,
    mut stream: EventStream,
    replay: Vec<Event>,
    mut last_seq: u64,
    writer: StoreWriter,
    stop: CancellationToken,
) -> u64 {
    let mut written: u64 = 0;
    let mut since_prune: u64 = 0;
    let mut replay = Some(replay);
    loop {
        let mut batch: Vec<Event> = replay.take().unwrap_or_default();
        let stopping = if !batch.is_empty() {
            stop.is_cancelled()
        } else {
            tokio::select! {
            biased;
            item = stream.recv_item() => {
                match item {
                    Some(StreamItem::Event(event)) => batch.push(event),
                    Some(StreamItem::Lagged { missed }) => {
                        let recovered = bus.history_since(last_seq, BATCH * 16);
                        tracing::warn!(missed, recovered = recovered.len(), "event log lagged; recovering from the ring");
                        batch.extend(recovered);
                    }
                    None => break,
                }
                false
            }
            () = stop.cancelled() => true,
            }
        };
        while batch.len() < BATCH {
            match stream.try_recv() {
                Some(event) => batch.push(event),
                None => break,
            }
        }
        batch.retain(|e| e.seq > last_seq);
        batch.sort_by_key(|e| e.seq);
        batch.dedup_by_key(|e| e.seq);
        if let Some(last) = batch.last() {
            let count = batch.len() as u64;
            let top = last.seq;
            match write(&writer, batch).await {
                Ok(()) => {
                    last_seq = top;
                    written += count;
                    since_prune += count;
                }
                Err(err) => tracing::error!(%err, count, "events not persisted"),
            }
        }
        if since_prune >= PRUNE_EVERY {
            since_prune = 0;
            let pruned = writer
                .execute_timeout(WRITE_TIMEOUT, |conn| {
                    EventRepo::new(conn).prune_keep_last(KEEP_EVENTS)
                })
                .await;
            if let Err(err) = pruned {
                tracing::warn!(%err, "pruning events failed");
            }
        }
        if stopping {
            // Drain everything already published, then leave.
            let mut rest: Vec<Event> = Vec::new();
            while let Some(event) = stream.try_recv() {
                rest.push(event);
            }
            rest.extend(bus.history_since(last_seq, usize::MAX));
            rest.retain(|e| e.seq > last_seq);
            rest.sort_by_key(|e| e.seq);
            rest.dedup_by_key(|e| e.seq);
            for chunk in rest.chunks(BATCH) {
                let count = chunk.len() as u64;
                if let Err(err) = write(&writer, chunk.to_vec()).await {
                    tracing::error!(%err, count, "events not persisted at shutdown");
                    break;
                }
                written += count;
            }
            break;
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::event::EventKind;
    use forge_core::store::Store;

    #[tokio::test]
    async fn every_published_event_is_stored_including_after_stop() {
        let store = Store::open_in_memory().unwrap();
        let bus = EventBus::new();
        let log = EventLog::spawn(&bus, store.writer().clone());
        for i in 0..1_000 {
            bus.publish(EventKind::ConfigRendered, serde_json::json!({ "i": i }));
        }
        let written = log.stop(Duration::from_secs(10)).await;
        assert_eq!(written, 1_000);
        let (count, max) = store
            .reader()
            .query(|conn| {
                let repo = EventRepo::new(conn);
                Ok((repo.count()?, repo.max_seq()?))
            })
            .await
            .unwrap();
        assert_eq!((count, max), (1_000, 1_000));
    }

    #[tokio::test]
    async fn bus_seeded_from_the_table_continues_the_sequence() {
        let store = Store::open_in_memory().unwrap();
        let bus = EventBus::new();
        let log = EventLog::spawn(&bus, store.writer().clone());
        bus.publish(EventKind::DaemonReady, serde_json::json!({}));
        bus.publish(EventKind::DaemonShutdown, serde_json::json!({}));
        log.stop(Duration::from_secs(5)).await;
        let max = store
            .reader()
            .query(|conn| EventRepo::new(conn).max_seq())
            .await
            .unwrap();
        let next = EventBus::starting_at(16, max + 1);
        assert_eq!(
            next.publish(EventKind::DaemonReady, serde_json::json!({}))
                .seq,
            3
        );
    }
}
