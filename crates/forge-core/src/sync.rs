//! Short-lived locks with a documented, debug-checked order (docs/03 §7.2, ADR-0011).
//!
//! Shared state lives in actors. The few mutexes that remain guard critical sections of
//! nanoseconds to microseconds (sequence counters, small tables, connection pools) and
//! are all [`OrderedMutex`]es. Each one has a [`LockLevel`]; a thread may only acquire a
//! lock whose rank is **strictly higher** than the rank of every lock it already holds.
//! Locks of the same rank can therefore never be nested. Debug builds verify this with a
//! thread-local and `debug_assert!`; release builds compile down to a plain
//! `parking_lot::Mutex`.
//!
//! The global order is listed in [`crate::actor`] (module docs) and encoded by the
//! constants on [`LockLevel`].
//!
//! The guard is `!Send`, so it cannot live across an `.await` in a spawned task, and
//! clippy rejects holding it across any `.await` (`await_holding_invalid_type`, configured
//! in `clippy.toml`).

use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

/// Position of a lock in the global lock order. Higher ranks are acquired later
/// (innermost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockLevel {
    rank: u8,
    name: &'static str,
}

impl LockLevel {
    /// Children table of an [`crate::actor::ActorSupervisor`].
    pub const ACTOR_CHILDREN: LockLevel = LockLevel::new(10, "actor.children");
    /// Job table of a [`crate::jobs::JobRegistry`]. Publishes `job.*` events while held.
    pub const JOB_TABLE: LockLevel = LockLevel::new(10, "jobs.table");
    /// Idle read connections of a [`crate::store::StoreReader`].
    pub const STORE_READERS: LockLevel = LockLevel::new(10, "store.readers");
    /// Outgoing queue (responses + per-subscription stream queues) of one API connection
    /// (`forge_ipc` server).
    pub const IPC_OUTBOX: LockLevel = LockLevel::new(10, "ipc.outbox");
    /// Request-id cache of the API server (idempotent retries, ADR-0009 rule 4).
    pub const IPC_IDEMPOTENCY: LockLevel = LockLevel::new(10, "ipc.idempotency");
    /// Live connections of the API server.
    pub const IPC_CONNECTIONS: LockLevel = LockLevel::new(10, "ipc.connections");
    /// Connection slot, pending calls, routes and subscriptions of an API client.
    pub const IPC_CLIENT: LockLevel = LockLevel::new(10, "ipc.client");
    /// Sequence counter of the [`crate::event::EventBus`]. Innermost lock: nothing may be
    /// acquired while it is held.
    pub const EVENT_BUS_SEQ: LockLevel = LockLevel::new(200, "event_bus.seq");

    /// A level with `rank` (must be greater than zero) and a name used in diagnostics.
    pub const fn new(rank: u8, name: &'static str) -> Self {
        assert!(rank > 0, "rank 0 means \"no lock held\"");
        Self { rank, name }
    }

    /// Rank in the order (higher = acquired later).
    pub const fn rank(&self) -> u8 {
        self.rank
    }

    /// Diagnostic name.
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

#[cfg(debug_assertions)]
thread_local! {
    /// `(rank, name)` of the innermost lock held by this thread; rank 0 = none.
    static HELD: std::cell::Cell<(u8, &'static str)> = const { std::cell::Cell::new((0, "")) };
}

/// `parking_lot::Mutex` with a [`LockLevel`]. Never hold it across `.await`.
pub struct OrderedMutex<T> {
    level: LockLevel,
    inner: parking_lot::Mutex<T>,
}

impl<T> OrderedMutex<T> {
    /// Mutex at `level` holding `value`.
    pub const fn new(level: LockLevel, value: T) -> Self {
        Self {
            level,
            inner: parking_lot::Mutex::new(value),
        }
    }

    /// Level of this mutex.
    pub fn level(&self) -> LockLevel {
        self.level
    }

    /// Acquires the lock.
    ///
    /// # Panics
    /// In debug builds, if this thread already holds a lock whose rank is not lower than
    /// this one (lock order violation or nesting of same-rank locks).
    pub fn lock(&self) -> OrderedMutexGuard<'_, T> {
        #[cfg(debug_assertions)]
        let prev = {
            let prev = HELD.get();
            debug_assert!(
                prev.0 < self.level.rank,
                "lock order violation: acquiring `{}` (rank {}) while holding `{}` (rank {}); \
                 see the lock order in forge_core::actor",
                self.level.name,
                self.level.rank,
                prev.1,
                prev.0
            );
            prev
        };
        let guard = self.inner.lock();
        #[cfg(debug_assertions)]
        HELD.set((self.level.rank, self.level.name));
        OrderedMutexGuard {
            guard,
            #[cfg(debug_assertions)]
            prev,
            not_send: PhantomData,
        }
    }

    /// Consumes the mutex and returns the value.
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T> fmt::Debug for OrderedMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrderedMutex")
            .field("level", &self.level.name)
            .finish_non_exhaustive()
    }
}

/// Guard of an [`OrderedMutex`]. `!Send`; must not be held across `.await`.
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct OrderedMutexGuard<'a, T> {
    guard: parking_lot::MutexGuard<'a, T>,
    #[cfg(debug_assertions)]
    prev: (u8, &'static str),
    /// `!Send` regardless of parking_lot's `send_guard` feature: the held-lock tracking
    /// is per thread, and a guard must never travel across an `.await` to another worker.
    not_send: PhantomData<*const ()>,
}

impl<T> Deref for OrderedMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for OrderedMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for OrderedMutexGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        HELD.set(self.prev);
    }
}

impl<T: fmt::Debug> fmt::Debug for OrderedMutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.guard, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increasing_ranks_nest() {
        let outer = OrderedMutex::new(LockLevel::JOB_TABLE, 1);
        let inner = OrderedMutex::new(LockLevel::EVENT_BUS_SEQ, 2);
        let a = outer.lock();
        let b = inner.lock();
        assert_eq!(*a + *b, 3);
        drop(b);
        drop(a);
        // Released: the inner lock can now be taken alone, and again the outer one.
        drop(inner.lock());
        drop(outer.lock());
    }

    #[test]
    fn guard_restores_previous_level() {
        let outer = OrderedMutex::new(LockLevel::STORE_READERS, ());
        let inner = OrderedMutex::new(LockLevel::EVENT_BUS_SEQ, ());
        {
            let _a = outer.lock();
            {
                let _b = inner.lock();
            }
            // Dropping the inner guard restores rank 10, so another inner lock is fine.
            let _c = inner.lock();
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lock order violation")]
    fn same_rank_nesting_is_rejected_in_debug() {
        let a = OrderedMutex::new(LockLevel::JOB_TABLE, ());
        let b = OrderedMutex::new(LockLevel::ACTOR_CHILDREN, ());
        let _ga = a.lock();
        let _gb = b.lock();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lock order violation")]
    fn decreasing_rank_is_rejected_in_debug() {
        let bus = OrderedMutex::new(LockLevel::EVENT_BUS_SEQ, ());
        let table = OrderedMutex::new(LockLevel::JOB_TABLE, ());
        let _g = bus.lock();
        let _t = table.lock();
    }

    #[test]
    fn levels_are_ordered_as_documented() {
        for outer in [
            LockLevel::ACTOR_CHILDREN,
            LockLevel::JOB_TABLE,
            LockLevel::STORE_READERS,
        ] {
            assert!(
                outer.rank() < LockLevel::EVENT_BUS_SEQ.rank(),
                "{}",
                outer.name()
            );
        }
    }
}
