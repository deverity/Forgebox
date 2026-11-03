//! Persistence of the aggregator in SQLite (M2-E5-T7, docs/14 section 3.5, ADR-0044 section 3).
//!
//! Everything here is a function of the owner of the [`Aggregator`] (the metrics actor, T8):
//! this module has no task of its own and no clock; `now` comes from the caller, so tests
//! drive it with a simulated clock.
//!
//! - **Flush** ([`Persister::flush`]): once a minute. Closes what the clock passed, drains the
//!   outbox ([`Aggregator::take_minutes`]) and sends the minutes plus the tail checkpoints they
//!   cover to the store writer as **one operation** (one savepoint): either both are stored or
//!   neither. Histograms are encoded (HDR V2) on the blocking pool before the writer sees them;
//!   a minute already stored (late deltas, a minute resumed after a clock jump) is merged with
//!   the stored one, which is exact because deltas add up.
//! - **Idempotence.** Merging is additive, so a flush must never apply twice. A writer
//!   `Timeout` means "outcome unknown" (the operation may still commit), therefore every
//!   attempt is a `Batch` with an op id (ULID) that the operation records in `metrics_flush`
//!   in the same savepoint as its rows; a second run of the same id is skipped. A batch whose
//!   write failed stays queued with its id and is re-sent, as is, before anything newer.
//! - **Checkpoints.** A tail batch carries the file position after it. A position may only be
//!   stored once every record before it is in a stored minute, otherwise a crash would resume
//!   after records that were never persisted. [`Persister::observe`] queues each batch with the
//!   newest `end_ts` it holds; at flush time the longest prefix of batches whose newest record
//!   lies in a closed minute becomes the *safe position* of that file. A batch that straddles
//!   the edge (some records in a minute that just closed, the rest in the open one) is not in
//!   the prefix, yet its older records are in the minutes the same flush stores. So the flush
//!   stores, with the safe position, the `closed_edge` it used: on resume ([`Resume::keeps`])
//!   the re-read records older than that edge are dropped, being stored already. The result is
//!   exact per-minute counts after a kill for logs in time order; records that arrive after
//!   their minute was flushed (late) and were not flushed yet are lost with the crash.
//! - **Rollup** ([`rollup`]): every hour fully past is rebuilt from its `metrics_1m` rows
//!   (histograms merged, never percentiles averaged). The newest rolled hour and the one before
//!   are rebuilt on every run so late deltas are not missed; the rebuild replaces the row. The
//!   minute rows are read and merged on the blocking pool through [`StoreReader`] (decoded
//!   accumulators, one encode per hour row); the writer only gets short operations with the
//!   finished hour rows (one per hour), so it is never held for the CPU work.
//! - **Prune** ([`prune`]): `metrics_1m` 7 days, `metrics_1h` 90 days.
//! - **Rehydrate** ([`rehydrate`]): at boot the last 24 h of `metrics_1m` go back to tier 2 (the
//!   minutes and the hours above them) through [`Aggregator::restore_minute`]. The 10 s points of
//!   tier 1 are not persisted (a minute row cannot rebuild them); `series` over the restored
//!   window answers at 1 minute and 1 hour steps, as documented in docs/14.
//! - **Reads over 24 h** ([`query_range`]) come from SQLite: hour rows for the part older than
//!   the minute retention, minute rows for the rest.
//!
//! No SQLite call runs on a tokio worker: writes go through [`StoreWriter`], reads through
//! [`StoreReader`], and histogram (de)coding runs in `spawn_blocking`.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{RequestId, SiteId};
use forge_core::store::{
    DbConnection, MetricsRepo, MetricsRow, MetricsTier, StoreReader, StoreWriter, TailRow,
};
use jiff::Timestamp;

use crate::agg::{Aggregator, Counters, MinuteDelta, Summary};
use crate::hist::{FORMAT_VERSION, LatencyHist};
use crate::record::AccessRecord;
use crate::tail::{Checkpoint, TailBatch};

/// How long `metrics_1m` rows are kept.
pub const RETENTION_1M: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long `metrics_1h` rows are kept.
pub const RETENTION_1H: Duration = Duration::from_secs(90 * 24 * 3600);
/// What boot puts back in memory.
pub const REHYDRATE_WINDOW: Duration = Duration::from_secs(24 * 3600);

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * 3600;

fn floor_to(secs: i64, step: i64) -> i64 {
    secs.div_euclid(step) * step
}

fn ceil_to(secs: i64, step: i64) -> i64 {
    -floor_to(-secs, step)
}

fn secs(ts: Timestamp) -> i64 {
    ts.as_second()
}

fn at(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn dur_secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX / 4)
}

fn hist_err(e: impl std::fmt::Display) -> ForgeError {
    ForgeError::Internal(format!("metrics histogram: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Rows <-> deltas

fn encode(h: &LatencyHist) -> Result<Vec<u8>> {
    if h.is_empty() {
        Ok(Vec::new())
    } else {
        h.to_v2().map_err(hist_err)
    }
}

fn decode(bytes: &[u8], version: u32) -> Result<LatencyHist> {
    if bytes.is_empty() {
        return Ok(LatencyHist::new());
    }
    if version != FORMAT_VERSION {
        return Err(ForgeError::Internal(format!(
            "metrics histogram format_version {version} is not supported (known: {FORMAT_VERSION})"
        )));
    }
    LatencyHist::from_v2(bytes).map_err(hist_err)
}

/// The row of a minute delta (encodes the histograms).
fn delta_row(d: &MinuteDelta) -> Result<MetricsRow> {
    Ok(MetricsRow {
        site: d.site,
        start: floor_to(secs(d.start), MINUTE),
        requests: d.counters.requests,
        status: d.counters.status,
        bytes: d.counters.bytes,
        rt_sum_us: d.counters.rt_sum_us,
        ut_sum_us: d.counters.ut_sum_us,
        ut_count: d.counters.ut_count,
        sampled: d.sampled,
        rt_hist: encode(&d.rt)?,
        ut_hist: encode(&d.ut)?,
        format_version: FORMAT_VERSION,
    })
}

fn counters_of(row: &MetricsRow) -> Counters {
    Counters {
        requests: row.requests,
        status: row.status,
        bytes: row.bytes,
        rt_sum_us: row.rt_sum_us,
        ut_sum_us: row.ut_sum_us,
        ut_count: row.ut_count,
    }
}

fn add_counters(a: &mut Counters, b: &Counters) {
    a.requests += b.requests;
    for (x, y) in a.status.iter_mut().zip(b.status) {
        *x += y;
    }
    a.bytes += b.bytes;
    a.rt_sum_us += b.rt_sum_us;
    a.ut_sum_us += b.ut_sum_us;
    a.ut_count += b.ut_count;
}

/// A stored row back as a delta for [`Aggregator::restore_minute`].
fn row_delta(row: &MetricsRow) -> Result<MinuteDelta> {
    Ok(MinuteDelta {
        site: row.site,
        start: at(row.start),
        counters: counters_of(row),
        sampled: row.sampled,
        rt: decode(&row.rt_hist, row.format_version)?,
        ut: decode(&row.ut_hist, row.format_version)?,
        complete: true,
    })
}

/// Sums two rows of the same key; histograms merge exactly.
fn merge_rows(base: &MetricsRow, add: &MetricsRow) -> Result<MetricsRow> {
    let mut counters = counters_of(base);
    add_counters(&mut counters, &counters_of(add));
    let rt = decode(&base.rt_hist, base.format_version)?
        .merge(&decode(&add.rt_hist, add.format_version)?);
    let ut = decode(&base.ut_hist, base.format_version)?
        .merge(&decode(&add.ut_hist, add.format_version)?);
    Ok(MetricsRow {
        site: base.site,
        start: base.start,
        requests: counters.requests,
        status: counters.status,
        bytes: counters.bytes,
        rt_sum_us: counters.rt_sum_us,
        ut_sum_us: counters.ut_sum_us,
        ut_count: counters.ut_count,
        sampled: base.sampled || add.sampled,
        rt_hist: encode(&rt)?,
        ut_hist: encode(&ut)?,
        format_version: FORMAT_VERSION,
    })
}

// ---------------------------------------------------------------------------------------------
// Flush

/// A batch of the tailer waiting for its records to be stored.
#[derive(Debug, Clone, Copy)]
struct Pending {
    /// Newest `end_ts` (seconds) among the records of the batch; `None` when it has none.
    newest: Option<i64>,
    checkpoint: Checkpoint,
}

/// What one flush did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushReport {
    /// Minute rows written (inserted or merged).
    pub rows: usize,
    /// Of those, rows merged into a stored row.
    pub merged: usize,
    /// Tail checkpoints written.
    pub checkpoints: usize,
    /// Time the operation held the writer thread.
    pub writer_time: Duration,
    /// Batches skipped because an earlier attempt had committed them (after a timeout).
    pub replayed: usize,
}

/// What [`Persister::tick`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub flush: FlushReport,
    pub rollup: Option<RollupReport>,
    pub prune: Option<PruneReport>,
}

/// One flush attempt: the encoded rows and positions, with the op id that makes the write
/// idempotent. Kept in the queue until the writer confirms it (or says it ran before).
#[derive(Debug, Clone)]
struct Batch {
    op_id: String,
    rows: Vec<MetricsRow>,
    tails: Vec<(String, TailRow)>,
    /// Unix seconds recorded with the op id (for the prune).
    at: i64,
}

/// Owner-side state of the persistence: minutes not stored yet and tail positions waiting for
/// their minutes.
#[derive(Debug, Default)]
pub struct Persister {
    /// Deltas that could not be encoded, tried again with the next flush.
    carry: Vec<MinuteDelta>,
    /// Batches whose write failed, re-sent (same op id) before anything newer.
    queue: VecDeque<Batch>,
    /// Writer timeout of a flush operation (`None`: the store default).
    write_timeout: Option<Duration>,
    /// Latest safe position per tail key (rewritten, with the new edge, at every flush).
    safe: BTreeMap<String, Checkpoint>,
    /// Batches in arrival order per tail key.
    pending: BTreeMap<String, VecDeque<Pending>>,
    last_rollup_hour: Option<i64>,
    last_prune_day: Option<i64>,
}

impl Persister {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the timeout of the writer operation of a flush (default: the store's write
    /// timeout). A timeout is "outcome unknown"; the retry is idempotent.
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = Some(timeout);
        self
    }

    /// Notes a batch handed to the aggregator: `key` is the tailer key, `newest` the latest
    /// `end_ts` among its records. Call it in the same step as `Aggregator::record`.
    pub fn observe(
        &mut self,
        key: &str,
        checkpoint: Option<Checkpoint>,
        newest: Option<Timestamp>,
    ) {
        let Some(checkpoint) = checkpoint else { return };
        self.pending
            .entry(key.to_owned())
            .or_default()
            .push_back(Pending {
                newest: newest.map(secs),
                checkpoint,
            });
    }

    /// [`Self::observe`] for a decoded batch.
    pub fn observe_batch(&mut self, batch: &TailBatch<AccessRecord>) {
        let newest = batch.items.iter().map(|r| r.end_ts).max();
        self.observe(&batch.key, batch.checkpoint, newest);
    }

    /// Minutes waiting for a write (after a failed one).
    pub fn carried(&self) -> usize {
        self.carry.len() + self.queue.iter().map(|b| b.rows.len()).sum::<usize>()
    }

    /// Batches queued for a checkpoint (diagnostics).
    pub fn pending_batches(&self) -> usize {
        self.pending.values().map(VecDeque::len).sum()
    }

    /// Moves the clock, drains the outbox and decides which positions are safe. Synchronous and
    /// quick: no batch can be observed in between, which is what makes the positions exact.
    fn prepare(&mut self, agg: &mut Aggregator, now: Timestamp) -> (Vec<MinuteDelta>, i64) {
        agg.advance(now);
        // Late records of closed minutes are folded into the outbox by queries; do it now so
        // that no observed record is left outside it.
        let sites: Vec<SiteId> = agg.sites().collect();
        for site in sites {
            let _ = agg.sums(site, now, now);
        }
        let edge = secs(agg.closed_minute_edge());
        let mut deltas = std::mem::take(&mut self.carry);
        deltas.extend(agg.take_minutes());
        for (key, queue) in &mut self.pending {
            let mut last = None;
            while queue
                .front()
                .is_some_and(|p| p.newest.is_none_or(|n| n < edge))
            {
                last = queue.pop_front().map(|p| p.checkpoint);
            }
            if let Some(cp) = last {
                self.safe.insert(key.clone(), cp);
            }
        }
        self.pending.retain(|_, q| !q.is_empty());
        (deltas, edge)
    }

    /// Stores the closed minutes and the covered positions in one writer operation. On error
    /// nothing is lost: the batch stays queued with its op id and is re-sent by the next flush,
    /// which is safe even when the failure was a timeout of an operation that did commit.
    pub async fn flush(
        &mut self,
        agg: &mut Aggregator,
        writer: &StoreWriter,
        now: Timestamp,
    ) -> Result<FlushReport> {
        let (deltas, edge) = self.prepare(agg, now);
        if !(deltas.is_empty() && self.safe.is_empty()) {
            let tails: Vec<(String, TailRow)> = self
                .safe
                .iter()
                .map(|(k, c)| (k.clone(), tail_row(c, edge)))
                .collect();
            // Encoding is CPU work proportional to the histograms: off the tokio workers. The
            // deltas come back with the rows so a failure never loses them.
            let (rows, deltas) = tokio::task::spawn_blocking(move || {
                let rows: Result<Vec<MetricsRow>> = deltas.iter().map(delta_row).collect();
                (rows, deltas)
            })
            .await
            .map_err(|e| ForgeError::Internal(format!("metrics encode task failed: {e}")))?;
            match rows {
                Ok(rows) => self.queue.push_back(Batch {
                    op_id: RequestId::new().to_string(),
                    rows,
                    tails,
                    at: secs(now),
                }),
                Err(err) => {
                    self.carry = deltas;
                    return Err(err);
                }
            }
        }
        let mut total = FlushReport::default();
        while let Some(batch) = self.queue.front() {
            let report = send_batch(writer, self.write_timeout, batch.clone(), now).await?;
            self.queue.pop_front();
            total.rows += report.rows;
            total.merged += report.merged;
            total.checkpoints += report.checkpoints;
            total.writer_time += report.writer_time;
            total.replayed += report.replayed;
        }
        Ok(total)
    }

    /// One scheduler step: a flush every call, plus the hourly rollup and the daily prune when
    /// the clock entered a new hour/day since they last ran (the first call runs both).
    pub async fn tick(
        &mut self,
        agg: &mut Aggregator,
        reader: &StoreReader,
        writer: &StoreWriter,
        now: Timestamp,
    ) -> Result<TickReport> {
        let flush = self.flush(agg, writer, now).await?;
        let mut report = TickReport {
            flush,
            ..TickReport::default()
        };
        let mut first_err = None;
        let hour = floor_to(secs(now), HOUR);
        if self.last_rollup_hour != Some(hour) {
            // A failed rollup must not starve the prune: run both, then report the first error.
            match rollup(reader, writer, now).await {
                Ok(r) => {
                    report.rollup = Some(r);
                    self.last_rollup_hour = Some(hour);
                }
                Err(err) => first_err = Some(err),
            }
        }
        let day = floor_to(secs(now), DAY);
        if self.last_prune_day != Some(day) {
            match prune(writer, now).await {
                Ok(p) => {
                    report.prune = Some(p);
                    self.last_prune_day = Some(day);
                }
                Err(err) => {
                    first_err.get_or_insert(err);
                }
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(report),
        }
    }
}

/// One writer operation: claims the op id, then merges the rows and stores the positions. An
/// op id seen before means an earlier attempt committed the whole batch: nothing to do.
async fn send_batch(
    writer: &StoreWriter,
    timeout: Option<Duration>,
    batch: Batch,
    now: Timestamp,
) -> Result<FlushReport> {
    let stamp = now.to_string();
    let op = move |conn: &mut DbConnection| {
        let started = Instant::now();
        let repo = MetricsRepo::new(conn);
        let mut report = FlushReport::default();
        if !repo.claim_flush(&batch.op_id, batch.at)? {
            report.replayed = 1;
            report.writer_time = started.elapsed();
            return Ok(report);
        }
        for row in &batch.rows {
            match repo.get(MetricsTier::Minute, row.site, row.start)? {
                None => repo.put(MetricsTier::Minute, row)?,
                Some(old) => {
                    let merged = match merge_rows(&old, row) {
                        Ok(m) => m,
                        Err(err) => {
                            tracing::warn!(%err, site = %row.site, start = row.start,
                                "stored metrics minute unreadable, replaced");
                            row.clone()
                        }
                    };
                    repo.put(MetricsTier::Minute, &merged)?;
                    report.merged += 1;
                }
            }
            report.rows += 1;
        }
        for (key, tail) in &batch.tails {
            repo.put_tail(key, tail, &stamp)?;
            report.checkpoints += 1;
        }
        report.writer_time = started.elapsed();
        Ok(report)
    };
    match timeout {
        Some(t) => writer.execute_timeout(t, op).await,
        None => writer.execute(op).await,
    }
}

fn tail_row(c: &Checkpoint, closed_edge: i64) -> TailRow {
    TailRow {
        closed_edge,
        creation_time: c.creation_time,
        offset: c.offset,
        head_len: c.head_len,
        head_hash: c.head_hash,
    }
}

/// Where to resume one tail after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resume {
    /// Start the tailer here (`StartAt::Checkpoint`).
    pub checkpoint: Checkpoint,
    /// Records older than this that the tailer re-reads are in the stored minutes already.
    pub closed_edge: Timestamp,
}

impl Resume {
    /// Whether a re-read record still has to be counted.
    pub fn keeps(&self, record: &AccessRecord) -> bool {
        record.end_ts >= self.closed_edge
    }

    /// Drops the records [`Self::keeps`] refuses.
    pub fn retain(&self, records: &mut Vec<AccessRecord>) {
        records.retain(|r| self.keeps(r));
    }
}

/// Positions stored by earlier flushes, to start the tailers with `StartAt::Checkpoint`.
pub async fn load_checkpoints(reader: &StoreReader) -> Result<Vec<(String, Resume)>> {
    reader
        .query(|conn| {
            Ok(MetricsRepo::new(conn)
                .tails()?
                .into_iter()
                .map(|(key, t)| {
                    (
                        key,
                        Resume {
                            checkpoint: Checkpoint {
                                creation_time: t.creation_time,
                                offset: t.offset,
                                head_len: t.head_len,
                                head_hash: t.head_hash,
                            },
                            closed_edge: at(t.closed_edge),
                        },
                    )
                })
                .collect())
        })
        .await
}

// ---------------------------------------------------------------------------------------------
// Rollup and prune

/// What a rollup did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RollupReport {
    /// Hour rows written.
    pub hours: usize,
    /// Minute rows read.
    pub minutes: usize,
    /// Minute rows left out because they could not be decoded.
    pub skipped: usize,
    /// Writer operations made (one per hour that has rows).
    pub writer_ops: usize,
    /// Longest time one writer operation held the writer thread.
    pub writer_time_max: Duration,
    /// Total time the writer operations held the writer thread.
    pub writer_time: Duration,
}

/// What a prune did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub minutes: usize,
    pub hours: usize,
}

/// Hours read and merged per reader call (bounds the memory of a catch-up).
const ROLLUP_CHUNK_HOURS: i64 = 6;

/// Rebuilds the hour rows of every hour fully past at `now`, from the first one not rolled yet
/// (the newest rolled hour and the one before are rebuilt: late deltas). Idempotent. Hours whose
/// minutes are partly pruned are never rebuilt.
///
/// The CPU work (decode, exact merge, one encode per hour row) runs in `reader` calls on the
/// blocking pool; the writer only receives the finished hour rows, one short operation per hour
/// (docs/03 section 7.1: short transactions).
pub async fn rollup(
    reader: &StoreReader,
    writer: &StoreWriter,
    now: Timestamp,
) -> Result<RollupReport> {
    let now_s = secs(now);
    let mut report = RollupReport::default();
    let end = floor_to(now_s, HOUR);
    let plan = reader
        .query(move |conn| {
            let repo = MetricsRepo::new(conn);
            let Some(oldest) = repo.oldest(MetricsTier::Minute)? else {
                return Ok(None);
            };
            // First hour whose 7 days of minutes are complete.
            let complete_from = ceil_to(now_s - dur_secs(RETENTION_1M), HOUR);
            let mut start = floor_to(oldest, HOUR).max(complete_from);
            if let Some(newest) = repo.newest(MetricsTier::Hour)? {
                start = start.max(newest - HOUR);
            }
            Ok(Some(start))
        })
        .await?;
    let Some(mut hour) = plan else {
        return Ok(report);
    };
    while hour < end {
        let chunk_end = (hour + ROLLUP_CHUNK_HOURS * HOUR).min(end);
        let (hours, minutes, skipped) = reader
            .query(move |conn| {
                let rows =
                    MetricsRepo::new(conn).range(MetricsTier::Minute, None, hour, chunk_end)?;
                Ok(merge_hours(rows))
            })
            .await?;
        report.minutes += minutes;
        report.skipped += skipped;
        for (_, rows) in hours {
            let n = rows.len();
            let spent = writer
                .execute(move |conn| {
                    let started = Instant::now();
                    let repo = MetricsRepo::new(conn);
                    for row in &rows {
                        repo.put(MetricsTier::Hour, row)?;
                    }
                    Ok(started.elapsed())
                })
                .await?;
            report.hours += n;
            report.writer_ops += 1;
            report.writer_time += spent;
            report.writer_time_max = report.writer_time_max.max(spent);
        }
        hour = chunk_end;
    }
    Ok(report)
}

/// Exact running total of one (hour, site): decoded histograms stay decoded until the end.
struct HourAcc {
    counters: Counters,
    sampled: bool,
    rt: LatencyHist,
    ut: LatencyHist,
}

/// Merges minute rows into hour rows, grouped by hour. Returns `(hour, rows)` in time order,
/// the number of minute rows read and the number skipped as unreadable (they must not block the
/// hour, nor the prune that follows). Blocking work: call it on the blocking pool.
fn merge_hours(minutes: Vec<MetricsRow>) -> (Vec<(i64, Vec<MetricsRow>)>, usize, usize) {
    let read = minutes.len();
    let mut skipped = 0;
    let mut acc: BTreeMap<(i64, SiteId), HourAcc> = BTreeMap::new();
    for row in &minutes {
        let (rt, ut) = match (
            decode(&row.rt_hist, row.format_version),
            decode(&row.ut_hist, row.format_version),
        ) {
            (Ok(rt), Ok(ut)) => (rt, ut),
            (Err(err), _) | (_, Err(err)) => {
                tracing::warn!(%err, site = %row.site, start = row.start,
                    "stored metrics minute skipped by rollup");
                skipped += 1;
                continue;
            }
        };
        let entry = acc
            .entry((floor_to(row.start, HOUR), row.site))
            .or_insert_with(|| HourAcc {
                counters: Counters::default(),
                sampled: false,
                rt: LatencyHist::new(),
                ut: LatencyHist::new(),
            });
        add_counters(&mut entry.counters, &counters_of(row));
        entry.sampled |= row.sampled;
        entry.rt = entry.rt.merge(&rt);
        entry.ut = entry.ut.merge(&ut);
    }
    let mut hours: Vec<(i64, Vec<MetricsRow>)> = Vec::new();
    for ((hour, site), a) in acc {
        let (Ok(rt_hist), Ok(ut_hist)) = (encode(&a.rt), encode(&a.ut)) else {
            tracing::warn!(%site, hour, "hour histogram could not be encoded, hour row skipped");
            continue;
        };
        let row = MetricsRow {
            site,
            start: hour,
            requests: a.counters.requests,
            status: a.counters.status,
            bytes: a.counters.bytes,
            rt_sum_us: a.counters.rt_sum_us,
            ut_sum_us: a.counters.ut_sum_us,
            ut_count: a.counters.ut_count,
            sampled: a.sampled,
            rt_hist,
            ut_hist,
            format_version: FORMAT_VERSION,
        };
        match hours.last_mut() {
            Some((h, rows)) if *h == hour => rows.push(row),
            _ => hours.push((hour, vec![row])),
        }
    }
    (hours, read, skipped)
}

/// Deletes minutes older than [`RETENTION_1M`] and hours older than [`RETENTION_1H`] at `now`.
pub async fn prune(writer: &StoreWriter, now: Timestamp) -> Result<PruneReport> {
    let now_s = secs(now);
    writer
        .execute(move |conn| {
            let repo = MetricsRepo::new(conn);
            repo.prune_flushes(floor_to(now_s - dur_secs(RETENTION_1M), MINUTE))?;
            Ok(PruneReport {
                minutes: repo.prune(
                    MetricsTier::Minute,
                    floor_to(now_s - dur_secs(RETENTION_1M), MINUTE),
                )?,
                hours: repo.prune(
                    MetricsTier::Hour,
                    floor_to(now_s - dur_secs(RETENTION_1H), HOUR),
                )?,
            })
        })
        .await
}

// ---------------------------------------------------------------------------------------------
// Rehydrate and reads

/// What a rehydration did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RehydrateReport {
    /// Minutes put back.
    pub restored: usize,
    /// Rows skipped because they could not be decoded.
    pub skipped: usize,
}

/// Puts the minutes of the last `window` (at most [`RETENTION_1M`]) back into the aggregator.
/// Call it right after `Aggregator::new(config, now)`, before records arrive.
pub async fn rehydrate(
    agg: &mut Aggregator,
    reader: &StoreReader,
    now: Timestamp,
    window: Duration,
) -> Result<RehydrateReport> {
    agg.advance(now);
    let to = floor_to(secs(now), MINUTE) + MINUTE;
    let from = floor_to(secs(now) - dur_secs(window.min(RETENTION_1M)), MINUTE);
    let (deltas, skipped) = reader
        .query(move |conn| {
            let rows = MetricsRepo::new(conn).range(MetricsTier::Minute, None, from, to)?;
            let mut deltas = Vec::with_capacity(rows.len());
            let mut skipped = 0;
            for row in &rows {
                match row_delta(row) {
                    Ok(d) => deltas.push(d),
                    Err(err) => {
                        tracing::warn!(%err, site = %row.site, start = row.start,
                            "stored metrics minute skipped");
                        skipped += 1;
                    }
                }
            }
            Ok((deltas, skipped))
        })
        .await?;
    let restored = deltas.len();
    for (i, d) in deltas.into_iter().enumerate() {
        agg.restore_minute(d);
        if i % 64 == 63 {
            // The aggregator belongs to the caller's task: do not hold its worker for 24 h of
            // 50 sites in one go.
            tokio::task::yield_now().await;
        }
    }
    Ok(RehydrateReport { restored, skipped })
}

/// Totals of `[from, to)` for `site` from SQLite (ranges over 24 h): hour rows for the hours
/// older than the minute retention, minute rows for the rest. Bounds align outward to the row
/// size (`Summary.from`/`to` say what was covered), clamped to `now` and to the stored history.
pub async fn query_range(
    reader: &StoreReader,
    site: SiteId,
    from: Timestamp,
    to: Timestamp,
    now: Timestamp,
) -> Result<Summary> {
    let now_s = secs(now);
    let to_s = secs(to).min(now_s);
    let from_s = secs(from).min(to_s);
    reader
        .query(move |conn| {
            let repo = MetricsRepo::new(conn);
            // First hour that has all its minutes: older time is read from the hour rows.
            let split = ceil_to(now_s - dur_secs(RETENTION_1M), HOUR);
            let min_from = floor_to(from_s, MINUTE);
            let min_to = ceil_to(to_s, MINUTE);
            let mut rows = Vec::new();
            let (mut covered_from, mut covered_to) = (min_from, min_from);
            if min_from < split {
                let hour_from = floor_to(from_s, HOUR);
                let hour_to = ceil_to(to_s, HOUR).min(split);
                if hour_to > hour_from {
                    rows.extend(repo.range(MetricsTier::Hour, Some(site), hour_from, hour_to)?);
                    covered_from = hour_from;
                    covered_to = hour_to;
                }
            }
            let minute_from = min_from.max(split);
            if min_to > minute_from {
                rows.extend(repo.range(MetricsTier::Minute, Some(site), minute_from, min_to)?);
                if covered_to <= covered_from {
                    covered_from = minute_from;
                }
                covered_to = min_to;
            }
            let mut counters = Counters::default();
            let (mut rt, mut ut) = (LatencyHist::new(), LatencyHist::new());
            let mut sampled = false;
            for row in &rows {
                let (row_rt, row_ut) = match (
                    decode(&row.rt_hist, row.format_version),
                    decode(&row.ut_hist, row.format_version),
                ) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(err), _) | (_, Err(err)) => {
                        tracing::warn!(%err, site = %row.site, start = row.start,
                            "stored metrics row skipped by query");
                        continue;
                    }
                };
                add_counters(&mut counters, &counters_of(row));
                sampled |= row.sampled;
                rt = rt.merge(&row_rt);
                ut = ut.merge(&row_ut);
            }
            Ok(Summary {
                from: at(covered_from),
                to: at(covered_to),
                counters,
                rt: rt.percentiles(),
                ut: ut.percentiles(),
                sampled,
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_helpers_handle_negatives_and_edges() {
        assert_eq!(floor_to(125, 60), 120);
        assert_eq!(ceil_to(121, 60), 180);
        assert_eq!(ceil_to(120, 60), 120);
        assert_eq!(floor_to(-1, 60), -60);
    }

    #[test]
    fn unknown_format_version_is_an_error_not_garbage() {
        let err = decode(&[1, 2, 3], FORMAT_VERSION + 1).unwrap_err();
        assert_eq!(err.code(), "internal");
        assert!(decode(&[], 99).unwrap().is_empty());
    }

    #[test]
    fn observe_keeps_arrival_order_and_ignores_missing_files() {
        let mut p = Persister::new();
        let cp = Checkpoint {
            creation_time: 1,
            offset: 10,
            head_len: 10,
            head_hash: 5,
        };
        p.observe("a", None, None);
        assert_eq!(p.pending_batches(), 0);
        p.observe("a", Some(cp), Some(at(100)));
        p.observe("a", Some(cp), None);
        assert_eq!(p.pending_batches(), 2);
    }
}
