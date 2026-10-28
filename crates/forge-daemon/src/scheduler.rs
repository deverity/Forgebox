//! Periodic work of the daemon (docs/03 §6) and its reactions to events. Each loop is a
//! task with a jittered interval; every call it makes has a timeout; network work in the
//! background respects `[updates] metered` and `quiet_hours`.
//!
//! | task | interval | notes |
//! |---|---|---|
//! | update checks | per tool: `next_check_at` of the update engine (`check_interval` ± 10 %, quiet hours, metered) | paused while a user job runs; `run_scheduled` applies automatic updates as jobs; each check publishes `tool.updates_available` ([`crate::update_feed`]) |
//! | registry refresh | 12 h ± 10 % | skipped on a metered connection (`metered = "pause"`) and in quiet hours |
//! | `tmp/` cleanup | 24 h ± 10 % | `tmp/stage/*` older than 24 h, `tmp/downloads/*` older than 7 days |
//! | port conflicts | 60 s ± 10 % | `port.conflict` once per new intruder |
//! | update list | on events | `tool.updated|rollback` → the applied update leaves the list at once, a re-check follows, and `tool.updates_available` is published (ADR-0039) |
//! | site/TLS reactions | on events | `site.*`, `tool.updated|rollback|current_changed`, `tls.renewed|leaf_issued` → pools + web servers re-rendered (debounced 300 ms); while a TLD is in `hosts` mode, the `hosts` block follows the sites and aliases (single flight, ADR-0033) |
//!
//! Certificate renewal (`tls` actor), health probes and process metrics (instance
//! actors) and pool sampling (pool actors) schedule themselves.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use forge_core::Timestamp;
use forge_core::config::{ForgeConfig, MeteredPolicy};
use forge_core::event::{EventKind, StreamItem};
use forge_core::ids::ToolId;
use forge_core::model::port::OwnerKind;
use forge_ipc::api::PortConflictPayload;
use forge_ipc::api::ToolVersionChangePayload;
use forge_net::ports::PortConflict;
use jiff::tz::TimeZone;
use rand::Rng;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::daemon::Daemon;
use crate::update_feed::{EngineSource, UpdateFeed, UpdateSource};

/// Registry refresh interval.
pub const REGISTRY_REFRESH: Duration = Duration::from_secs(12 * 3600);
/// `tmp/` cleanup interval.
pub const TMP_CLEANUP: Duration = Duration::from_secs(24 * 3600);
/// Port conflict check interval.
pub const PORT_CHECK: Duration = Duration::from_secs(60);
/// Longest sleep of the update loop between two looks at the schedule.
const UPDATE_MAX_SLEEP: Duration = Duration::from_secs(600);
/// Debounce of the event reactions.
const REACT_DEBOUNCE: Duration = Duration::from_millis(300);

/// `base` ± `fraction` (uniform).
pub fn jittered(base: Duration, fraction: f64) -> Duration {
    let fraction = fraction.clamp(0.0, 0.9);
    let factor = rand::thread_rng().gen_range((1.0 - fraction)..=(1.0 + fraction));
    base.mul_f64(factor)
}

/// Whether background network work must wait now (metered connection with
/// `metered = "pause"`, or inside `quiet_hours`). Blocking (COM call): use from
/// `spawn_blocking`.
pub fn background_paused(config: &ForgeConfig, metered: bool) -> Option<&'static str> {
    if metered && config.updates.metered == MeteredPolicy::Pause {
        return Some("metered");
    }
    if let Some(quiet) = &config.updates.quiet_hours {
        let local = Timestamp::now().to_zoned(TimeZone::system());
        let minute =
            u16::from(local.hour().unsigned_abs()) * 60 + u16::from(local.minute().unsigned_abs());
        if quiet.contains(minute).unwrap_or(false) {
            return Some("quiet_hours");
        }
    }
    None
}

/// The scheduler's tasks.
#[derive(Debug)]
pub struct Scheduler {
    stop: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl Scheduler {
    /// Starts every loop, with the update engine of the daemon as the source of updates.
    pub fn start(daemon: &Daemon) -> Self {
        Self::start_with(
            daemon,
            Arc::new(EngineSource {
                engine: daemon.updates.clone(),
                reader: daemon.reader.clone(),
            }),
        )
    }

    /// Starts every loop with `source` answering the update questions (a fake in tests).
    pub fn start_with(daemon: &Daemon, source: Arc<dyn UpdateSource>) -> Self {
        let stop = CancellationToken::new();
        let feed = Arc::new(UpdateFeed::new(daemon.bus.clone()));
        // The feed starts from the last persisted list, before either loop publishes, so
        // no publication after a restart is partial or empty (ADR-0039 §3).
        let reader = daemon.reader.clone();
        let updates_daemon = daemon.clone();
        let updates_stop = stop.clone();
        let tasks = vec![
            tokio::spawn(async move {
                feed.seed_from_store(&reader).await;
                tokio::join!(
                    update_loop(
                        updates_daemon.clone(),
                        Arc::clone(&source),
                        Arc::clone(&feed),
                        updates_stop.clone(),
                    ),
                    update_react_loop(updates_daemon, source, feed, updates_stop),
                );
            }),
            tokio::spawn(registry_loop(daemon.clone(), stop.clone())),
            tokio::spawn(tmp_loop(daemon.clone(), stop.clone())),
            tokio::spawn(port_loop(daemon.clone(), stop.clone())),
            tokio::spawn(react_loop(daemon.clone(), stop.clone())),
        ];
        Self { stop, tasks }
    }

    /// Stops every loop (the work in flight is dropped; jobs it started keep their own
    /// lifecycle and are cancelled by the shutdown).
    pub async fn stop(self, timeout: Duration) {
        self.stop.cancel();
        for task in self.tasks {
            if tokio::time::timeout(timeout, task).await.is_err() {
                tracing::warn!("a scheduler task did not stop in time");
            }
        }
    }
}

async fn sleep_or_stop(stop: &CancellationToken, wait: Duration) -> bool {
    tokio::select! {
        () = stop.cancelled() => false,
        () = tokio::time::sleep(wait) => true,
    }
}

async fn is_metered(daemon: &Daemon) -> bool {
    let cost = daemon.network_cost.clone();
    forge_core::runtime::spawn_blocking_timeout("network cost", Duration::from_secs(5), move || {
        cost.is_metered()
    })
    .await
    .unwrap_or(false)
}

/// Whether a job started by a user (not by the scheduler) is running.
fn user_job_running(daemon: &Daemon) -> bool {
    daemon
        .jobs
        .active()
        .iter()
        .any(|job| job.request_id.is_some())
}

/// One due background check of `tool`: runs it, keeps what it found in `feed` and
/// publishes `tool.updates_available` (after every check that ran, changed or not).
/// A deferred check (quiet hours, metered) changes nothing.
pub(crate) async fn run_due_check(source: &dyn UpdateSource, feed: &UpdateFeed, tool: &ToolId) {
    match source.run_scheduled(tool, Timestamp::now()).await {
        Ok(run) => {
            tracing::info!(%tool, deferred = ?run.deferred, updates = run.updates.len(), started = run.started.len(), "scheduled update check");
            if run.deferred.is_none() {
                feed.set_tool(tool, &run.updates);
                feed.publish();
            }
        }
        Err(err) => tracing::warn!(%tool, %err, "scheduled update check failed"),
    }
}

async fn update_loop(
    daemon: Daemon,
    source: Arc<dyn UpdateSource>,
    feed: Arc<UpdateFeed>,
    stop: CancellationToken,
) {
    let mut next: BTreeMap<ToolId, Option<Timestamp>> = BTreeMap::new();
    loop {
        let tools: Vec<ToolId> = match source.installed_tools().await {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(%err, "scheduler: cannot list installed tools");
                Vec::new()
            }
        };
        if feed.retain_tools(&tools) {
            // A tool was removed: its updates leave the list.
            feed.publish();
        }
        // `manual` tools (no next check) are asked again: the policy may have changed.
        next.retain(|_, at| at.is_some());
        let now = Timestamp::now();
        for tool in &tools {
            if next.contains_key(tool) {
                continue;
            }
            match source.next_check_at(tool, now).await {
                Ok(check) => {
                    next.insert(tool.clone(), check.at);
                }
                Err(err) => tracing::debug!(%tool, %err, "scheduler: no update schedule"),
            }
        }
        next.retain(|tool, _| tools.contains(tool));
        let due: Vec<ToolId> = next
            .iter()
            .filter(|(_, at)| at.is_some_and(|at| at <= now))
            .map(|(tool, _)| tool.clone())
            .collect();
        if !due.is_empty() && user_job_running(&daemon) {
            tracing::debug!("scheduler: user job running; update checks deferred");
        } else {
            for tool in due {
                run_due_check(source.as_ref(), &feed, &tool).await;
                next.remove(&tool);
            }
        }
        let now = Timestamp::now();
        let wait = next
            .values()
            .flatten()
            .map(|at| {
                let secs = at.as_second().saturating_sub(now.as_second()).max(1);
                Duration::from_secs(u64::try_from(secs).unwrap_or(1))
            })
            .min()
            .unwrap_or(UPDATE_MAX_SLEEP)
            .min(UPDATE_MAX_SLEEP);
        if !sleep_or_stop(&stop, wait).await {
            return;
        }
    }
}

/// What `tool.updated|rollback` changes in the list of pending updates. The applied
/// update leaves the list at once (the badge must not wait for a network check); then the
/// tool is checked again (a newer release may exist, or the rolled-back version becomes
/// the update to offer) unless the connection is metered or in quiet hours.
async fn update_react_loop(
    daemon: Daemon,
    source: Arc<dyn UpdateSource>,
    feed: Arc<UpdateFeed>,
    stop: CancellationToken,
) {
    let mut events = daemon.bus.subscribe();
    loop {
        let item = tokio::select! {
            () = stop.cancelled() => return,
            item = events.recv_item() => item,
        };
        let event = match item {
            Some(StreamItem::Event(event)) => event,
            Some(StreamItem::Lagged { .. }) => continue,
            None => return,
        };
        if !matches!(event.kind, EventKind::ToolUpdated | EventKind::ToolRollback) {
            continue;
        }
        let Ok(change) = serde_json::from_value::<ToolVersionChangePayload>(event.payload) else {
            continue;
        };
        if event.kind == EventKind::ToolUpdated {
            feed.remove_series(&change.tool, change.series.as_deref());
            feed.publish();
        }
        let metered = is_metered(&daemon).await;
        if let Some(why) = background_paused(&daemon.config(), metered) {
            tracing::debug!(tool = %change.tool, why, "update list: re-check postponed");
            if event.kind == EventKind::ToolRollback {
                feed.publish();
            }
            continue;
        }
        match source.check(&change.tool).await {
            Ok(found) => {
                let changed = feed.set_tool(&change.tool, &found);
                if changed || event.kind == EventKind::ToolRollback {
                    feed.publish();
                }
            }
            Err(err) => {
                tracing::debug!(tool = %change.tool, %err, "update list: re-check failed");
                if event.kind == EventKind::ToolRollback {
                    feed.publish();
                }
            }
        }
    }
}

async fn registry_loop(daemon: Daemon, stop: CancellationToken) {
    loop {
        if !sleep_or_stop(&stop, jittered(REGISTRY_REFRESH, 0.1)).await {
            return;
        }
        let metered = is_metered(&daemon).await;
        if let Some(why) = background_paused(&daemon.config(), metered) {
            tracing::info!(why, "scheduler: registry refresh postponed");
            continue;
        }
        for info in daemon.registry.list() {
            if !info.enabled {
                continue;
            }
            match daemon.registry.refresh(&info.name) {
                Ok(job) => {
                    tracing::info!(registry = %info.name, %job, "scheduled registry refresh")
                }
                Err(err) => {
                    tracing::warn!(registry = %info.name, %err, "registry refresh not started")
                }
            }
        }
    }
}

/// Removes entries of `dir` older than `age` (files and folders). Blocking.
fn remove_older_than(dir: &Path, age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|elapsed| elapsed > age);
        if !old {
            continue;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed += 1,
            Err(e) => tracing::debug!(path = %path.display(), %e, "tmp cleanup skipped an entry"),
        }
    }
    removed
}

async fn tmp_loop(daemon: Daemon, stop: CancellationToken) {
    loop {
        if !sleep_or_stop(&stop, jittered(TMP_CLEANUP, 0.1)).await {
            return;
        }
        if !daemon.jobs.active().is_empty() {
            // Stage folders of running installations must stay.
            continue;
        }
        let (stage, downloads) = (daemon.root.tmp_stage(), daemon.root.tmp_downloads());
        let result = forge_core::runtime::spawn_blocking_timeout(
            "tmp cleanup",
            Duration::from_secs(300),
            move || {
                Ok(remove_older_than(&stage, Duration::from_secs(24 * 3600))
                    + remove_older_than(&downloads, Duration::from_secs(7 * 24 * 3600)))
            },
        )
        .await;
        match result {
            Ok(n) => tracing::info!(removed = n, "tmp cleanup"),
            Err(err) => tracing::warn!(%err, "tmp cleanup failed"),
        }
    }
}

async fn port_loop(daemon: Daemon, stop: CancellationToken) {
    loop {
        if !sleep_or_stop(&stop, jittered(PORT_CHECK, 0.1)).await {
            return;
        }
        // Our own listeners are the processes in the root Job Object.
        let ours: Vec<u32> = {
            let tree = daemon.job_tree.clone();
            forge_core::runtime::spawn_blocking_timeout(
                "listing supervised pids",
                Duration::from_secs(5),
                move || tree.pids(),
            )
            .await
            .unwrap_or_default()
        };
        let own_pid = std::process::id();
        let instances = crate::api::instance_names(&daemon);
        let result = daemon
            .ports
            .conflicts(|_owner, pid| pid == own_pid || ours.contains(&pid))
            .await;
        match result {
            Ok(conflicts) => {
                for conflict in conflicts.into_iter().filter(|c| c.newly_detected) {
                    let payload = conflict_payload(&conflict, &instances);
                    tracing::warn!(
                        port = payload.port,
                        pid = conflict.holder.pid,
                        owner = %payload.owner_name,
                        "port conflict"
                    );
                    daemon.bus.publish(
                        EventKind::PortConflict,
                        serde_json::to_value(payload).unwrap_or_default(),
                    );
                }
            }
            Err(err) => tracing::debug!(%err, "port conflict check failed"),
        }
    }
}

/// The `port.conflict` payload of a conflict. `owner_name` is `<tool>/<instance>`
/// (`mysql/default`) for an instance, `<kind>/<id>` for any other owner (ADR-0039).
pub(crate) fn conflict_payload(
    conflict: &PortConflict,
    instances: &BTreeMap<String, String>,
) -> PortConflictPayload {
    let r = &conflict.reservation;
    PortConflictPayload {
        v: 1,
        port: r.port,
        protocol: forge_ipc::api::PortProtocol::Tcp,
        owner_name: crate::api::owner_name(instances, &r.owner),
        instance_id: (r.owner.kind == OwnerKind::Instance)
            .then(|| r.owner.id.parse().ok())
            .flatten(),
        holder: Some(forge_ipc::api::PortHolderDto {
            pid: conflict.holder.pid,
            exe: conflict
                .holder
                .exe_path
                .as_deref()
                .map(forge_core::paths::posix_string)
                .unwrap_or_else(|| "?".into()),
        }),
    }
}

fn triggers_reconcile(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::SiteCreated
            | EventKind::SiteUpdated
            | EventKind::SiteRemoved
            | EventKind::SitePhpChanged
            | EventKind::ToolUpdated
            | EventKind::ToolRollback
            | EventKind::ToolInstallCompleted
            | EventKind::TlsRenewed
    )
}

async fn react_loop(daemon: Daemon, stop: CancellationToken) {
    let mut events = daemon.bus.subscribe();
    let hosts_sync = Arc::new(HostsSync::default());
    // Once at start: hosts mode restored at boot follows the sites as they are now.
    sync_dns_hosts(&daemon, &hosts_sync);
    loop {
        let item = tokio::select! {
            () = stop.cancelled() => return,
            item = events.recv_item() => item,
        };
        let kind = match item {
            Some(StreamItem::Event(event)) => Some(event.kind),
            Some(StreamItem::Lagged { .. }) => Some(EventKind::SiteUpdated),
            None => return,
        };
        if !kind.is_some_and(triggers_reconcile) {
            continue;
        }
        // Debounce: wait for the burst to end.
        loop {
            let more = tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(REACT_DEBOUNCE) => false,
                item = events.recv_item() => item.is_some(),
            };
            if !more {
                break;
            }
        }
        if daemon.shutdown.is_requested() {
            return;
        }
        if let Err(err) = daemon.web.reconcile("event").await {
            tracing::warn!(%err, "reconcile after event failed");
        }
        sync_dns_hosts(&daemon, &hosts_sync);
    }
}

/// While a TLD is in `hosts` mode (docs/06 §5.4, ADR-0033), the block follows the sites
/// and aliases. Runs apart from the reaction loop (a write waits for a UAC prompt); one
/// sync at a time, and a change during a sync runs one more. `DnsPolicyManager` serializes
/// it with `dns enable/disable`, so a sync never writes the block back after a disable.
fn sync_dns_hosts(daemon: &Daemon, state: &Arc<HostsSync>) {
    if !daemon.config().dns.enabled || daemon.dns_policy.hosts_mode_tlds().is_empty() {
        return;
    }
    if !state.request() {
        return;
    }
    let daemon = daemon.clone();
    let state = Arc::clone(state);
    tokio::spawn(async move {
        while state.next() {
            if daemon.shutdown.is_requested() {
                state.release();
                break;
            }
            let synced = match crate::api::net::site_hosts(&daemon).await {
                Ok(hosts) => daemon.dns_policy.sync_sites(&hosts).await,
                Err(err) => Err(err),
            };
            match synced {
                Ok(true) => tracing::info!("dns: hosts block updated for the sites"),
                Ok(false) => {}
                Err(err) => tracing::warn!(%err, "dns: hosts block not updated"),
            }
        }
    });
}

/// Single-flight flag of [`sync_dns_hosts`]: one worker at a time, and a request that
/// lands while it runs (even while it is about to stop) gets one more pass.
#[derive(Debug, Default)]
struct HostsSync {
    running: AtomicBool,
    dirty: AtomicBool,
}

impl HostsSync {
    /// Records a change; `true` when the caller must start the worker (none running).
    fn request(&self) -> bool {
        self.dirty.store(true, Ordering::SeqCst);
        !self.running.swap(true, Ordering::SeqCst)
    }

    /// Worker side: `true` when there is a change to sync; `false` once the worker has
    /// stopped. Before stopping it looks again: a request that came after the last check
    /// but saw `running` still set would otherwise be lost.
    fn next(&self) -> bool {
        self.next_with(|| {})
    }

    /// [`Self::next`] with `window` run between the last empty check and the stop (tests
    /// put a concurrent request there).
    fn next_with(&self, window: impl Fn()) -> bool {
        loop {
            if self.dirty.swap(false, Ordering::SeqCst) {
                return true;
            }
            window();
            self.running.store(false, Ordering::SeqCst);
            if !self.dirty.load(Ordering::SeqCst) || self.running.swap(true, Ordering::SeqCst) {
                return false;
            }
        }
    }

    /// Worker side: stops without syncing what is pending (shutdown).
    fn release(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::config::QuietHours;
    use forge_core::model::port::ReservationOwner;

    #[test]
    fn jitter_stays_in_bounds() {
        for _ in 0..1_000 {
            let d = jittered(Duration::from_secs(100), 0.1);
            assert!(
                d >= Duration::from_secs(90) && d <= Duration::from_secs(110),
                "{d:?}"
            );
        }
        assert_eq!(
            jittered(Duration::from_secs(5), 0.0),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn metered_pause_and_quiet_hours_postpone() {
        let mut config = ForgeConfig::default();
        assert_eq!(background_paused(&config, true), Some("metered"));
        assert_eq!(background_paused(&config, false), None);
        config.updates.metered = MeteredPolicy::Allow;
        assert_eq!(background_paused(&config, true), None);
        config.updates.quiet_hours = Some(QuietHours {
            start: "00:00".into(),
            end: "23:59".into(),
        });
        // Almost the whole day is quiet: only 23:59 local is outside.
        let local = Timestamp::now().to_zoned(TimeZone::system());
        if !(local.hour() == 23 && local.minute() == 59) {
            assert_eq!(background_paused(&config, false), Some("quiet_hours"));
        }
    }

    #[test]
    fn old_tmp_entries_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fresh.part"), b"x").unwrap();
        assert_eq!(remove_older_than(dir.path(), Duration::from_secs(3600)), 0);
        assert_eq!(remove_older_than(dir.path(), Duration::ZERO), 1);
        assert_eq!(
            remove_older_than(&dir.path().join("missing"), Duration::ZERO),
            0
        );
    }

    #[test]
    fn hosts_sync_never_loses_a_request() {
        // A burst of requests from several threads, each caller running the worker when
        // `request` says so: once everything is done no change is left unsynced and no
        // worker is marked running. The lost wakeup (`dirty` set while the worker was
        // between its last check and `running = false`) leaves `dirty` true here.
        for _ in 0..20 {
            let state = Arc::new(HostsSync::default());
            let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let state = Arc::clone(&state);
                    let runs = Arc::clone(&runs);
                    std::thread::spawn(move || {
                        for _ in 0..2_000 {
                            if state.request() {
                                while state.next() {
                                    runs.fetch_add(1, Ordering::SeqCst);
                                    std::hint::spin_loop();
                                }
                            }
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            assert!(!state.dirty.load(Ordering::SeqCst), "a request was lost");
            assert!(!state.running.load(Ordering::SeqCst));
            assert!(runs.load(Ordering::SeqCst) >= 1);
        }
        // The race itself: a request lands after the worker found nothing to do but before
        // it stopped; it sees the worker running and leaves, so the worker must take it.
        let state = HostsSync::default();
        assert!(state.request());
        assert!(state.next());
        let late = std::cell::Cell::new(None);
        assert!(state.next_with(|| {
            if late.get().is_none() {
                late.set(Some(state.request()));
            }
        }));
        assert_eq!(
            late.get(),
            Some(false),
            "the late caller did not start a worker"
        );
        assert!(!state.next());
        assert!(!state.dirty.load(Ordering::SeqCst));
        assert!(!state.running.load(Ordering::SeqCst));

        // Sequential: every request is served, a request during a run gets one more.
        let state = HostsSync::default();
        assert!(state.request());
        assert!(state.next());
        assert!(!state.request(), "worker running");
        assert!(state.next(), "the request made during the run");
        assert!(!state.next());
        assert!(state.request(), "stopped: the next caller starts it");
        state.release();
        assert!(state.request());
    }

    fn conflict(owner: ReservationOwner) -> PortConflict {
        use forge_core::model::port::{PortReservation, PortState};
        use forge_core::platform::{PortOwner, PortProto};
        let mut reservation = PortReservation::new_tcp(3306, None, owner);
        reservation.state = PortState::Conflict;
        PortConflict {
            reservation,
            holder: PortOwner {
                port: 3306,
                addr: "0.0.0.0".parse().unwrap(),
                pid: 4412,
                exe_path: Some(r"C:\xampp\mysql\bin\mysqld.exe".into()),
                proto: PortProto::Tcp,
            },
            newly_detected: true,
        }
    }

    #[test]
    fn port_conflict_names_the_owner_as_tool_slash_instance() {
        let id = forge_core::ids::InstanceId::new();
        let owner = ReservationOwner::new(OwnerKind::Instance, id.to_string(), "main").unwrap();
        let names = BTreeMap::from([(id.to_string(), "mysql/default".to_owned())]);
        let payload = conflict_payload(&conflict(owner.clone()), &names);
        assert_eq!(payload.owner_name, "mysql/default");
        assert_eq!(payload.instance_id, Some(id));
        assert_eq!(payload.port, 3306);
        let holder = payload.holder.unwrap();
        assert_eq!(holder.pid, 4412);
        assert_eq!(holder.exe, "C:/xampp/mysql/bin/mysqld.exe");
        // The payload on the wire.
        let json =
            serde_json::to_value(conflict_payload(&conflict(owner.clone()), &names)).unwrap();
        assert_eq!(json["owner_name"], "mysql/default");
        // An instance that is gone, or another kind of owner, falls back to kind/id.
        let unknown = conflict_payload(&conflict(owner), &BTreeMap::new());
        assert_eq!(unknown.owner_name, format!("instance/{id}"));
        let daemon = ReservationOwner::new(OwnerKind::Daemon, "dns", "main").unwrap();
        let payload = conflict_payload(&conflict(daemon), &names);
        assert_eq!(payload.owner_name, "daemon/dns");
        assert_eq!(payload.instance_id, None);
    }

    #[tokio::test]
    async fn a_due_check_publishes_the_whole_list_and_a_deferred_one_nothing() {
        use crate::update_feed::testing::{FakeSource, update};
        let bus = forge_core::event::EventBus::new();
        let mut events = bus.subscribe();
        let feed = UpdateFeed::new(bus);
        let php = ToolId::new("php").unwrap();
        let source = FakeSource::with(vec![
            update("php", "8.3", "8.3.36", "8.3.37"),
            update("php", "8.4", "8.4.1", "8.4.2"),
        ]);
        run_due_check(&source, &feed, &php).await;
        let event = events.try_recv().expect("tool.updates_available");
        assert_eq!(event.kind, EventKind::ToolUpdatesAvailable);
        assert_eq!(event.payload["count"], 2);
        // Nothing found any more: the list is published empty (clears the badge).
        source.found.lock().unwrap().clear();
        run_due_check(&source, &feed, &php).await;
        let event = events.try_recv().expect("an empty list is still published");
        assert_eq!(event.payload["count"], 0);
        assert_eq!(event.payload["updates"], serde_json::json!([]));
        assert!(events.try_recv().is_none());
        // A check deferred (metered connection, quiet hours) did not look: nothing changes
        // and nothing is published.
        *source.deferred.lock().unwrap() = Some(forge_provider::update::DeferReason::Metered);
        run_due_check(&source, &feed, &php).await;
        assert!(events.try_recv().is_none());
    }

    #[test]
    fn reconcile_triggers() {
        assert!(triggers_reconcile(EventKind::SiteCreated));
        assert!(triggers_reconcile(EventKind::TlsRenewed));
        assert!(!triggers_reconcile(EventKind::JobProgress));
        assert!(!triggers_reconcile(EventKind::ConfigRendered));
    }
}
