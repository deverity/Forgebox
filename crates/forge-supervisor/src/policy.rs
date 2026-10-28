//! Restart policies (docs/03 §1.4): what happens after an instance crashed.
//!
//! * [`RestartPolicy::Never`]: it stays `Crashed`.
//! * [`RestartPolicy::OnFailure`]: restarts after a non-zero exit or after being stopped
//!   for failing health checks, at most `max` times within the sliding `window`; the next
//!   failure inside the window gives up (`Failed(CrashLoop)`). A clean exit (code 0)
//!   stays `Crashed`.
//! * [`RestartPolicy::Always`]: restarts after any unexpected exit, without limit.
//!
//! Every automatic restart waits an exponential backoff: `min`, `2·min`, `4·min`… capped
//! at `max` (1 s .. 30 s by default), where the exponent is the number of automatic
//! restarts in the window (for `Always`, in [`ALWAYS_BACKOFF_WINDOW`]): a service that
//! stayed up longer than the window starts again from `min`. Exit codes that no restart
//! can fix (missing DLL, access denied) never restart, whatever the policy.
//!
//! [`RestartTracker`] is pure (time is passed in), so the rules are unit-tested without
//! processes or timers. A manual start resets it.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

/// Exponent window of [`RestartPolicy::Always`] (it has no `max`/`window` of its own).
pub const ALWAYS_BACKOFF_WINDOW: Duration = Duration::from_secs(60);

/// Exponential delay before an automatic restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backoff {
    pub min: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    /// `1s .. 30s` (docs/03 §1.4).
    fn default() -> Self {
        Self {
            min: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    /// Delay before the restart that follows `previous` restarts: `min · 2^previous`,
    /// capped at `max` (and never below `min`).
    pub fn delay(&self, previous: u32) -> Duration {
        let factor = 2_u32.saturating_pow(previous.min(31));
        self.min
            .checked_mul(factor)
            .unwrap_or(self.max)
            .min(self.max)
            .max(self.min)
    }
}

/// Restart policy of an instance (manifest `service.daemon.restart_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure {
        /// Automatic restarts allowed within `window` (0 = none).
        max: u32,
        window: Duration,
        backoff: Backoff,
    },
    Always {
        backoff: Backoff,
    },
}

impl Default for RestartPolicy {
    /// `on-failure`, 3 restarts in 60 s, backoff 1 s .. 30 s (the manifest default).
    fn default() -> Self {
        Self::OnFailure {
            max: 3,
            window: Duration::from_secs(60),
            backoff: Backoff::default(),
        }
    }
}

/// How a run ended without being asked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The main process exited with this code.
    Exit(u32),
    /// It was stopped after `failure_threshold` failed health checks.
    Unhealthy,
}

/// What to do after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Start again after `delay`; this is automatic restart number `attempt` (1-based)
    /// of the current window.
    Restart { delay: Duration, attempt: u32 },
    /// The policy does not restart this kind of ending: stay `Crashed`.
    Stay,
    /// The budget is spent: `restarts` automatic restarts already happened within
    /// `window`.
    GiveUp { restarts: u32, window: Duration },
}

/// Automatic restarts of one instance (timestamps inside the current window).
#[derive(Debug, Clone, Default)]
pub struct RestartTracker {
    recent: VecDeque<Instant>,
    total: u32,
}

impl RestartTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decides what follows `failure` at `now` and, for a restart, records it.
    pub fn decide(
        &mut self,
        policy: &RestartPolicy,
        failure: Failure,
        now: Instant,
    ) -> RestartDecision {
        let (window, backoff, max) = match *policy {
            RestartPolicy::Never => return RestartDecision::Stay,
            RestartPolicy::OnFailure {
                max,
                window,
                backoff,
            } => {
                if failure == Failure::Exit(0) {
                    return RestartDecision::Stay;
                }
                (window, backoff, Some(max))
            }
            RestartPolicy::Always { backoff } => (ALWAYS_BACKOFF_WINDOW, backoff, None),
        };
        while let Some(&oldest) = self.recent.front() {
            if now.saturating_duration_since(oldest) >= window {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        let previous = u32::try_from(self.recent.len()).unwrap_or(u32::MAX);
        if let Some(max) = max {
            if previous >= max {
                return RestartDecision::GiveUp {
                    restarts: previous,
                    window,
                };
            }
        }
        self.recent.push_back(now);
        self.total = self.total.saturating_add(1);
        RestartDecision::Restart {
            delay: backoff.delay(previous),
            attempt: previous + 1,
        }
    }

    /// Forgets the history (a manual start gets a fresh budget).
    pub fn reset(&mut self) {
        self.recent.clear();
    }

    /// Automatic restarts since the instance was created.
    pub fn total(&self) -> u32 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let backoff = Backoff::default();
        let delays: Vec<u64> = (0..8).map(|n| backoff.delay(n).as_secs()).collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff.delay(u32::MAX), secs(30));
        let tiny = Backoff {
            min: Duration::from_millis(100),
            max: Duration::from_millis(250),
        };
        assert_eq!(tiny.delay(0), Duration::from_millis(100));
        assert_eq!(tiny.delay(1), Duration::from_millis(200));
        assert_eq!(tiny.delay(2), Duration::from_millis(250));
    }

    #[test]
    fn on_failure_restarts_up_to_max_then_gives_up() {
        let policy = RestartPolicy::default();
        let mut tracker = RestartTracker::new();
        let t0 = Instant::now();
        let decisions: Vec<RestartDecision> = (0..4)
            .map(|n| tracker.decide(&policy, Failure::Exit(3), t0 + secs(n)))
            .collect();
        assert_eq!(
            decisions,
            vec![
                RestartDecision::Restart {
                    delay: secs(1),
                    attempt: 1
                },
                RestartDecision::Restart {
                    delay: secs(2),
                    attempt: 2
                },
                RestartDecision::Restart {
                    delay: secs(4),
                    attempt: 3
                },
                RestartDecision::GiveUp {
                    restarts: 3,
                    window: secs(60)
                },
            ]
        );
        assert_eq!(tracker.total(), 3);
        // Unhealthy counts as a failure too.
        assert!(matches!(
            tracker.decide(&policy, Failure::Unhealthy, t0 + secs(5)),
            RestartDecision::GiveUp { .. }
        ));
    }

    #[test]
    fn window_slides_and_resets_the_backoff() {
        let policy = RestartPolicy::default();
        let mut tracker = RestartTracker::new();
        let t0 = Instant::now();
        for n in 0..3 {
            assert!(matches!(
                tracker.decide(&policy, Failure::Exit(1), t0 + secs(n)),
                RestartDecision::Restart { .. }
            ));
        }
        // 60 s after the first restart it leaves the window: one slot frees up.
        assert_eq!(
            tracker.decide(&policy, Failure::Exit(1), t0 + secs(60)),
            RestartDecision::Restart {
                delay: secs(4),
                attempt: 3
            }
        );
        // Long after everything: fresh window, backoff back to min.
        assert_eq!(
            tracker.decide(&policy, Failure::Exit(1), t0 + secs(600)),
            RestartDecision::Restart {
                delay: secs(1),
                attempt: 1
            }
        );
        tracker.reset();
        assert_eq!(
            tracker.decide(&policy, Failure::Exit(1), t0 + secs(601)),
            RestartDecision::Restart {
                delay: secs(1),
                attempt: 1
            }
        );
    }

    #[test]
    fn clean_exit_and_never_stay_crashed() {
        let t0 = Instant::now();
        let mut tracker = RestartTracker::new();
        assert_eq!(
            tracker.decide(&RestartPolicy::default(), Failure::Exit(0), t0),
            RestartDecision::Stay
        );
        assert_eq!(
            tracker.decide(&RestartPolicy::Never, Failure::Exit(3), t0),
            RestartDecision::Stay
        );
        assert_eq!(
            tracker.decide(&RestartPolicy::Never, Failure::Unhealthy, t0),
            RestartDecision::Stay
        );
        let none = RestartPolicy::OnFailure {
            max: 0,
            window: secs(60),
            backoff: Backoff::default(),
        };
        assert!(matches!(
            tracker.decide(&none, Failure::Exit(3), t0),
            RestartDecision::GiveUp { restarts: 0, .. }
        ));
        assert_eq!(tracker.total(), 0);
    }

    #[test]
    fn always_restarts_without_limit() {
        let policy = RestartPolicy::Always {
            backoff: Backoff::default(),
        };
        let mut tracker = RestartTracker::new();
        let t0 = Instant::now();
        let delays: Vec<u64> = (0..10)
            .map(
                |n| match tracker.decide(&policy, Failure::Exit(0), t0 + secs(n)) {
                    RestartDecision::Restart { delay, .. } => delay.as_secs(),
                    other => panic!("{other:?}"),
                },
            )
            .collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30, 30, 30]);
        assert_eq!(tracker.total(), 10);
    }

    #[test]
    fn policy_wire_format() {
        let json = serde_json::to_value(RestartPolicy::Never).unwrap();
        assert_eq!(json, serde_json::json!({"on": "never"}));
        let json = serde_json::to_value(RestartPolicy::default()).unwrap();
        assert_eq!(json["on"], "on-failure");
        assert_eq!(json["max"], 3);
        let back: RestartPolicy = serde_json::from_value(json).unwrap();
        assert_eq!(back, RestartPolicy::default());
    }
}
