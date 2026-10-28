//! `request_id` cache of the server (ADR-0009, rule 4): a retried mutating request
//! (same `request_id`, possibly on a new connection after a network drop) gets the
//! answer of the first execution instead of acting twice. A retry that arrives while
//! the first is still running waits for its answer.
//!
//! Outcomes `timeout` and `internal` are not cached (the operation may not have
//! happened; a retry runs it again; long jobs are themselves idempotent through
//! `JobRegistry`). Entries live `ttl` and at most `capacity` are kept.
//!
//! Lock: `ipc.idempotency` (rank 10), never held while a handler runs.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::watch;

use forge_core::ids::RequestId;
use forge_core::sync::{LockLevel, OrderedMutex};

use crate::api::ApiError;

pub(crate) type Outcome = Result<Value, ApiError>;

enum Entry {
    Running {
        method: &'static str,
        done: watch::Receiver<Option<Outcome>>,
    },
    Done {
        method: &'static str,
        outcome: Outcome,
        at: Instant,
    },
}

struct State {
    entries: HashMap<RequestId, Entry>,
    /// Insertion order, for eviction.
    order: VecDeque<RequestId>,
}

pub(crate) struct IdempotencyCache {
    state: OrderedMutex<State>,
    capacity: usize,
    ttl: Duration,
}

/// Decision for an incoming mutating request.
pub(crate) enum Begin<'a> {
    /// First time: run it, then [`Guard::complete`].
    Run(Guard<'a>),
    /// Already answered.
    Cached(Outcome),
    /// Running elsewhere: wait for this.
    Wait(watch::Receiver<Option<Outcome>>),
    /// The id was used for another method.
    Mismatch(&'static str),
}

/// Holder of a running entry; dropping it without completing forgets the entry.
pub(crate) struct Guard<'a> {
    cache: &'a IdempotencyCache,
    id: RequestId,
    sender: Option<watch::Sender<Option<Outcome>>>,
}

impl IdempotencyCache {
    pub(crate) fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            state: OrderedMutex::new(
                LockLevel::IPC_IDEMPOTENCY,
                State {
                    entries: HashMap::new(),
                    order: VecDeque::new(),
                },
            ),
            capacity: capacity.max(1),
            ttl,
        }
    }

    pub(crate) fn begin(&self, id: RequestId, method: &'static str) -> Begin<'_> {
        let mut state = self.state.lock();
        self.evict(&mut state);
        match state.entries.get(&id) {
            Some(Entry::Done {
                method: known,
                outcome,
                ..
            }) => {
                if *known != method {
                    return Begin::Mismatch(known);
                }
                return Begin::Cached(outcome.clone());
            }
            Some(Entry::Running {
                method: known,
                done,
            }) => {
                if *known != method {
                    return Begin::Mismatch(known);
                }
                return Begin::Wait(done.clone());
            }
            None => {}
        }
        let (sender, receiver) = watch::channel(None);
        state.entries.insert(
            id,
            Entry::Running {
                method,
                done: receiver,
            },
        );
        state.order.push_back(id);
        Begin::Run(Guard {
            cache: self,
            id,
            sender: Some(sender),
        })
    }

    fn evict(&self, state: &mut State) {
        let now = Instant::now();
        while let Some(id) = state.order.front().copied() {
            let expired = match state.entries.get(&id) {
                None => true,
                Some(Entry::Done { at, .. }) => {
                    now.duration_since(*at) > self.ttl || state.entries.len() > self.capacity
                }
                // Running entries are never evicted; stop at the first one.
                Some(Entry::Running { .. }) => false,
            };
            if !expired {
                break;
            }
            state.order.pop_front();
            if matches!(state.entries.get(&id), Some(Entry::Done { .. })) {
                state.entries.remove(&id);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state.lock().entries.len()
    }
}

fn cacheable(outcome: &Outcome) -> bool {
    !matches!(outcome, Err(e) if e.code == "timeout" || e.code == "internal")
}

impl Guard<'_> {
    /// Records the outcome and wakes the waiting retries.
    pub(crate) fn complete(mut self, outcome: Outcome) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        {
            let mut state = self.cache.state.lock();
            if cacheable(&outcome) {
                let method = match state.entries.get(&self.id) {
                    Some(Entry::Running { method, .. }) => *method,
                    _ => "",
                };
                state.entries.insert(
                    self.id,
                    Entry::Done {
                        method,
                        outcome: outcome.clone(),
                        at: Instant::now(),
                    },
                );
            } else {
                state.entries.remove(&self.id);
            }
        }
        let _ = sender.send(Some(outcome));
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.cache.state.lock().entries.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn second_call_gets_cached_outcome() {
        let cache = IdempotencyCache::new(16, Duration::from_secs(60));
        let id = RequestId::new();
        let Begin::Run(guard) = cache.begin(id, "sites.add") else {
            panic!("first call must run");
        };
        guard.complete(Ok(json!({"id": 1})));
        match cache.begin(id, "sites.add") {
            Begin::Cached(Ok(v)) => assert_eq!(v, json!({"id": 1})),
            _ => panic!("expected cached"),
        }
        assert!(matches!(
            cache.begin(id, "sites.remove"),
            Begin::Mismatch("sites.add")
        ));
    }

    #[tokio::test]
    async fn concurrent_retry_waits_for_the_first() {
        let cache = IdempotencyCache::new(16, Duration::from_secs(60));
        let id = RequestId::new();
        let Begin::Run(guard) = cache.begin(id, "db.drop") else {
            panic!()
        };
        let Begin::Wait(mut rx) = cache.begin(id, "db.drop") else {
            panic!("expected wait")
        };
        guard.complete(Err(ApiError::new("not_found", "no db")));
        let outcome = rx.wait_for(Option::is_some).await.unwrap().clone().unwrap();
        assert_eq!(outcome.unwrap_err().code, "not_found");
    }

    #[test]
    fn timeouts_and_abandoned_runs_are_forgotten() {
        let cache = IdempotencyCache::new(16, Duration::from_secs(60));
        let id = RequestId::new();
        let Begin::Run(guard) = cache.begin(id, "x.y") else {
            panic!()
        };
        guard.complete(Err(ApiError::new("timeout", "slow")));
        assert!(matches!(cache.begin(id, "x.y"), Begin::Run(_)));
        let other = RequestId::new();
        drop(cache.begin(other, "x.y"));
        assert!(matches!(cache.begin(other, "x.y"), Begin::Run(_)));
    }

    #[test]
    fn capacity_and_ttl_evict_done_entries() {
        let cache = IdempotencyCache::new(2, Duration::from_secs(60));
        for _ in 0..5 {
            if let Begin::Run(g) = cache.begin(RequestId::new(), "a.b") {
                g.complete(Ok(Value::Null));
            }
        }
        assert!(cache.len() <= 3, "{}", cache.len());
        let cache = IdempotencyCache::new(16, Duration::ZERO);
        let id = RequestId::new();
        if let Begin::Run(g) = cache.begin(id, "a.b") {
            g.complete(Ok(Value::Null));
        }
        std::thread::sleep(Duration::from_millis(5));
        assert!(matches!(cache.begin(id, "a.b"), Begin::Run(_)), "expired");
    }
}
