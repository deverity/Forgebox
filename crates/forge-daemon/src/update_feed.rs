//! `tool.updates_available` (docs/03 §8, ADR-0039): the daemon keeps the list of updates
//! the checks found, per tool, and publishes the **whole** list after every scheduled
//! check and after `tool.updated|rollback`, so a client (the GUI's badge, `forge status
//! --watch`) replaces what it shows instead of merging deltas. The event is stored with
//! the others, so a client reconnecting with `resume_from` gets the last list too.
//!
//! * [`UpdateSource`]: what the scheduler needs from the update engine; the real one is
//!   [`EngineSource`], tests inject a fake (no network, no installed tools).
//! * [`UpdateFeed`]: the per-tool state and the publishing. Blocked updates (a build that
//!   needs a migration the user must start) are not part of the list: nothing can be done
//!   about them from the badge.

use std::collections::BTreeMap;

use forge_core::Timestamp;
use forge_core::error::Result;
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::ToolId;
use forge_core::store::StoreReader;
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::api::{PendingUpdateDto, ToolUpdatesAvailablePayload};
use forge_provider::update::{AvailableUpdate, DeferReason, NextCheck, ScheduledRun, UpdateEngine};
use futures::future::BoxFuture;

use crate::catalog;

/// Version of the `tool.updates_available` payload.
pub const PAYLOAD_VERSION: u32 = 1;

/// What the scheduler asks of the update engine.
pub trait UpdateSource: Send + Sync + 'static {
    /// Tools with an installed version (the ones worth checking).
    fn installed_tools(&self) -> BoxFuture<'_, Result<Vec<ToolId>>>;

    /// When the next background check of `tool` should run.
    fn next_check_at<'a>(
        &'a self,
        tool: &'a ToolId,
        now: Timestamp,
    ) -> BoxFuture<'a, Result<NextCheck>>;

    /// A due background run of `tool`: checks, and starts the automatic updates.
    fn run_scheduled<'a>(
        &'a self,
        tool: &'a ToolId,
        now: Timestamp,
    ) -> BoxFuture<'a, Result<ScheduledRun>>;

    /// The updates available for `tool` right now (a check, no side effects).
    fn check<'a>(&'a self, tool: &'a ToolId) -> BoxFuture<'a, Result<Vec<AvailableUpdate>>>;
}

/// The real source: the update engine and the store's installed versions.
pub struct EngineSource {
    pub engine: UpdateEngine,
    pub reader: StoreReader,
}

impl UpdateSource for EngineSource {
    fn installed_tools(&self) -> BoxFuture<'_, Result<Vec<ToolId>>> {
        Box::pin(async move {
            let mut ids: Vec<ToolId> = catalog::all_installed(&self.reader)
                .await?
                .into_iter()
                .map(|r| r.tool_id)
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        })
    }

    fn next_check_at<'a>(
        &'a self,
        tool: &'a ToolId,
        now: Timestamp,
    ) -> BoxFuture<'a, Result<NextCheck>> {
        Box::pin(self.engine.next_check_at(tool, now))
    }

    fn run_scheduled<'a>(
        &'a self,
        tool: &'a ToolId,
        now: Timestamp,
    ) -> BoxFuture<'a, Result<ScheduledRun>> {
        Box::pin(self.engine.run_scheduled(tool, now))
    }

    fn check<'a>(&'a self, tool: &'a ToolId) -> BoxFuture<'a, Result<Vec<AvailableUpdate>>> {
        Box::pin(self.engine.check(Some(tool)))
    }
}

/// The entry of an update in the event (`None` for a blocked one).
pub fn pending(update: &AvailableUpdate) -> Option<PendingUpdateDto> {
    update.blocked.is_none().then(|| PendingUpdateDto {
        tool: update.tool.clone(),
        series: update.series.clone(),
        from: update.from.clone(),
        to: update.to.clone(),
    })
}

/// Pending updates per tool and their publishing. Cheap to share by reference.
pub struct UpdateFeed {
    bus: EventBus,
    // Lock: `daemon.tables` (leaf). Never held across an await or while publishing.
    pending: OrderedMutex<BTreeMap<ToolId, Vec<PendingUpdateDto>>>,
}

impl UpdateFeed {
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus,
            pending: OrderedMutex::new(LockLevel::DAEMON_TABLES, BTreeMap::new()),
        }
    }

    /// Replaces what a check found for `tool`. Returns whether the list changed.
    pub fn set_tool(&self, tool: &ToolId, found: &[AvailableUpdate]) -> bool {
        let mut list: Vec<PendingUpdateDto> = found.iter().filter_map(pending).collect();
        list.sort_by(|a, b| a.series.cmp(&b.series));
        let mut map = self.pending.lock();
        let changed = map.get(tool).map(Vec::as_slice).unwrap_or_default() != list.as_slice();
        if list.is_empty() {
            map.remove(tool);
        } else {
            map.insert(tool.clone(), list);
        }
        changed
    }

    /// Drops the pending update of one series (it was applied). Returns whether there was
    /// one.
    pub fn remove_series(&self, tool: &ToolId, series: Option<&str>) -> bool {
        let mut map = self.pending.lock();
        let Some(list) = map.get_mut(tool) else {
            return false;
        };
        let before = list.len();
        match series {
            Some(series) => list.retain(|u| u.series != series),
            None => list.clear(),
        }
        let changed = list.len() != before;
        if list.is_empty() {
            map.remove(tool);
        }
        changed
    }

    /// Forgets tools that are no longer installed. Returns whether anything went.
    pub fn retain_tools(&self, installed: &[ToolId]) -> bool {
        let mut map = self.pending.lock();
        let before = map.len();
        map.retain(|tool, _| installed.contains(tool));
        map.len() != before
    }

    /// Every pending update, by tool then series.
    pub fn snapshot(&self) -> Vec<PendingUpdateDto> {
        self.pending.lock().values().flatten().cloned().collect()
    }

    /// Publishes `tool.updates_available` with the whole list.
    pub fn publish(&self) -> ToolUpdatesAvailablePayload {
        let updates = self.snapshot();
        let payload = ToolUpdatesAvailablePayload {
            v: PAYLOAD_VERSION,
            count: u32::try_from(updates.len()).unwrap_or(u32::MAX),
            updates,
        };
        self.bus.publish(
            EventKind::ToolUpdatesAvailable,
            serde_json::to_value(&payload).unwrap_or_default(),
        );
        payload
    }
}

/// Fakes for the tests of this crate and its integration tests (no network, no installed
/// tools). Not part of the daemon's API.
#[doc(hidden)]
pub mod testing {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use forge_core::ids::VersionId;
    use forge_manifest::schema::tool::UpdatePolicy;
    use forge_provider::provider::Release;
    use forge_provider::update::UpdateKind;

    use super::*;

    /// An update `tool` `series`: `from` → `to`.
    pub fn update(tool: &str, series: &str, from: &str, to: &str) -> AvailableUpdate {
        AvailableUpdate {
            tool: ToolId::new(tool).unwrap(),
            series: series.into(),
            from: from.into(),
            from_id: VersionId::new(),
            from_canonical: from.into(),
            to: to.into(),
            to_canonical: to.into(),
            to_series: series.into(),
            kind: UpdateKind::Patch,
            policy: UpdatePolicy::Notify,
            automatic: false,
            pinned: false,
            installed: false,
            blocked: None,
            release: Release {
                version: to.into(),
                url: format!("https://example.test/{tool}-{to}.zip"),
                sha256: None,
                sha512: None,
                size: None,
                variants: BTreeMap::new(),
                published_at: None,
                signature: None,
                file_name: format!("{tool}-{to}.zip"),
                checksum_url: None,
                tags: Vec::new(),
            },
        }
    }

    /// A source whose answers the test sets.
    #[derive(Default)]
    pub struct FakeSource {
        pub tools: Mutex<Vec<ToolId>>,
        pub found: Mutex<BTreeMap<ToolId, Vec<AvailableUpdate>>>,
        pub checks: Mutex<Vec<ToolId>>,
        /// Answered as the reason a scheduled run did nothing (quiet hours, metered).
        pub deferred: Mutex<Option<DeferReason>>,
    }

    impl FakeSource {
        pub fn with(found: Vec<AvailableUpdate>) -> Self {
            let source = Self::default();
            for u in found {
                let mut tools = source.tools.lock().unwrap();
                if !tools.contains(&u.tool) {
                    tools.push(u.tool.clone());
                }
                drop(tools);
                source
                    .found
                    .lock()
                    .unwrap()
                    .entry(u.tool.clone())
                    .or_default()
                    .push(u);
            }
            source
        }
    }

    impl UpdateSource for FakeSource {
        fn installed_tools(&self) -> BoxFuture<'_, Result<Vec<ToolId>>> {
            Box::pin(async move { Ok(self.tools.lock().unwrap().clone()) })
        }

        fn next_check_at<'a>(
            &'a self,
            _tool: &'a ToolId,
            now: Timestamp,
        ) -> BoxFuture<'a, Result<NextCheck>> {
            // Due at once.
            Box::pin(async move {
                Ok(NextCheck {
                    at: Some(now),
                    deferred: None,
                })
            })
        }

        fn run_scheduled<'a>(
            &'a self,
            tool: &'a ToolId,
            _now: Timestamp,
        ) -> BoxFuture<'a, Result<ScheduledRun>> {
            Box::pin(async move {
                self.checks.lock().unwrap().push(tool.clone());
                Ok(ScheduledRun {
                    tool: tool.clone(),
                    deferred: *self.deferred.lock().unwrap(),
                    updates: self
                        .found
                        .lock()
                        .unwrap()
                        .get(tool)
                        .cloned()
                        .unwrap_or_default(),
                    started: Vec::new(),
                })
            })
        }

        fn check<'a>(&'a self, tool: &'a ToolId) -> BoxFuture<'a, Result<Vec<AvailableUpdate>>> {
            Box::pin(async move {
                self.checks.lock().unwrap().push(tool.clone());
                Ok(self
                    .found
                    .lock()
                    .unwrap()
                    .get(tool)
                    .cloned()
                    .unwrap_or_default())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::update;
    use super::*;

    fn tool(id: &str) -> ToolId {
        ToolId::new(id).unwrap()
    }

    #[test]
    fn publishes_the_whole_list_with_its_count() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let feed = UpdateFeed::new(bus);
        assert!(feed.set_tool(
            &tool("php"),
            &[
                update("php", "8.4", "8.4.1", "8.4.2"),
                update("php", "8.3", "8.3.36", "8.3.37"),
            ]
        ));
        assert!(feed.set_tool(
            &tool("nginx"),
            &[update("nginx", "1.28", "1.28.0", "1.28.1")]
        ));
        let payload = feed.publish();
        assert_eq!(payload.count, 3);
        let order: Vec<_> = payload
            .updates
            .iter()
            .map(|u| (u.tool.as_str(), u.series.as_str()))
            .collect();
        assert_eq!(order, [("nginx", "1.28"), ("php", "8.3"), ("php", "8.4")]);
        let event = events.try_recv().expect("event published");
        assert_eq!(event.kind, EventKind::ToolUpdatesAvailable);
        assert_eq!(event.payload["count"], 3);
        assert_eq!(event.payload["updates"][0]["from"], "1.28.0");
        assert_eq!(event.payload["updates"][0]["to"], "1.28.1");
    }

    #[test]
    fn blocked_updates_are_not_listed_and_changes_are_detected() {
        let feed = UpdateFeed::new(EventBus::new());
        let mut blocked = update("php", "8.4", "8.4.1", "8.4.2");
        blocked.blocked = Some("needs a migration".into());
        let a = update("php", "8.3", "8.3.36", "8.3.37");
        assert!(feed.set_tool(&tool("php"), &[a.clone(), blocked.clone()]));
        assert_eq!(feed.snapshot().len(), 1);
        assert!(!feed.set_tool(&tool("php"), &[a, blocked]), "same list");
        assert!(feed.set_tool(&tool("php"), &[]), "cleared");
        assert!(feed.snapshot().is_empty());
        assert!(!feed.set_tool(&tool("php"), &[]));
    }

    #[test]
    fn applied_updates_and_removed_tools_leave_the_list() {
        let feed = UpdateFeed::new(EventBus::new());
        feed.set_tool(
            &tool("php"),
            &[
                update("php", "8.3", "8.3.36", "8.3.37"),
                update("php", "8.4", "8.4.1", "8.4.2"),
            ],
        );
        feed.set_tool(
            &tool("nginx"),
            &[update("nginx", "1.28", "1.28.0", "1.28.1")],
        );
        assert!(feed.remove_series(&tool("php"), Some("8.3")));
        assert!(!feed.remove_series(&tool("php"), Some("8.3")));
        assert!(!feed.remove_series(&tool("mysql"), None));
        assert_eq!(feed.snapshot().len(), 2);
        assert!(feed.retain_tools(&[tool("php")]));
        let left = feed.snapshot();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].series, "8.4");
        assert!(feed.remove_series(&tool("php"), None));
        assert!(feed.snapshot().is_empty());
    }
}
