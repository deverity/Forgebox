//! Daemon auto-start policy of a context ([ADR-0034]): a pure state machine fed with
//! facts (opened, connected, lost, shutdown announced, start finished) and asked what to do
//! once the daemon has not answered for the grace period. No clock, no I/O: the caller
//! passes `now`, so every rule is tested with synthetic instants.
//!
//! Rules:
//! * A root that is not initialized (no `state/`) is never started automatically; the GUI
//!   routes to onboarding instead ([`Decision::Uninitialized`]).
//! * When the context opens, an initialized root whose daemon does not answer is started
//!   (the initial auto-start, same as `forge` does, [`AutoStartPolicy::on_open`]).
//! * After an *unexpected* loss (no `daemon.shutdown` announced): the daemon is restarted.
//!   Automatic restarts are spaced by at least `interval`, which starts at
//!   [`PolicyConfig::min_interval`] (60 s), doubles while restarted daemons die again
//!   before staying up for [`PolicyConfig::stable_after`] (crash loop; capped at
//!   [`PolicyConfig::max_interval`]) and goes back to 60 s once one stays up.
//! * After an *announced* shutdown: `restart`/`update` mean the daemon comes back by
//!   itself, so nothing is started for [`PolicyConfig::comeback`]; any other reason
//!   (`user`: `daemon.stop`, `forge daemon stop`, the tray; `signal`: Ctrl+C, logoff) is a
//!   deliberate stop and the daemon is never restarted automatically
//!   ([`Decision::StoppedOnPurpose`]) until the user starts it.
//! * A start the user asks for is never rate limited and clears a deliberate stop.
//!
//! [ADR-0034]: ../../../../../docs/adr/0034-desktop-shell-architecture.md

use std::time::Duration;

use tokio::time::Instant;

/// Timings of the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyConfig {
    /// How long a lost connection may stay lost before the policy is asked (lets a
    /// `daemon.shutdown` that raced the loss arrive, and a transient drop heal).
    pub grace: Duration,
    /// Minimum spacing between two automatic restarts.
    pub min_interval: Duration,
    /// Upper bound of the spacing while a crash loop goes on.
    pub max_interval: Duration,
    /// A daemon that stayed up this long ends a crash loop.
    pub stable_after: Duration,
    /// After `daemon.shutdown {reason: restart|update}`: how long to wait for the daemon to
    /// come back by itself before treating it as lost.
    pub comeback: Duration,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_millis(750),
            min_interval: Duration::from_secs(60),
            max_interval: Duration::from_secs(600),
            stable_after: Duration::from_secs(60),
            comeback: Duration::from_secs(15),
        }
    }
}

/// What the daemon announced before going away (`daemon.shutdown.reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announced {
    /// `restart` or `update`: it restarts itself.
    Restarting { at: Instant },
    /// Any other reason: stopped on purpose.
    Stopped { reason: String },
}

/// What to do about a daemon that does not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Start it now.
    Start,
    /// Nothing yet; ask again at this instant.
    CheckAt(Instant),
    /// Nothing: the root is not initialized (onboarding decides).
    Uninitialized,
    /// Nothing: the daemon was stopped on purpose; only the user starts it again.
    StoppedOnPurpose,
}

/// How a start that succeeded went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// A daemon was spawned and answered.
    Started,
    /// Somebody else's daemon answered (nothing was spawned).
    AlreadyRunning,
}

/// State of the policy of one context.
#[derive(Debug, Clone)]
pub struct AutoStartPolicy {
    config: PolicyConfig,
    announced: Option<Announced>,
    connected_since: Option<Instant>,
    /// Last automatic restart (or failed initial start) and the one before it.
    last_restart: Option<Instant>,
    previous_restart: Option<Instant>,
    /// Current spacing of automatic restarts.
    interval: Duration,
    /// The last connection stayed up for `stable_after`.
    stable: bool,
    restarts: u32,
}

impl AutoStartPolicy {
    pub fn new(config: PolicyConfig) -> Self {
        Self {
            config,
            announced: None,
            connected_since: None,
            last_restart: None,
            previous_restart: None,
            interval: config.min_interval,
            stable: false,
            restarts: 0,
        }
    }

    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// The context was opened: start the daemon if the root is initialized.
    pub fn on_open(&mut self, initialized: bool) -> Decision {
        if initialized {
            Decision::Start
        } else {
            Decision::Uninitialized
        }
    }

    /// A connection was authenticated.
    pub fn on_connected(&mut self, now: Instant) {
        self.announced = None;
        self.connected_since = Some(now);
    }

    /// `daemon.shutdown {reason}` arrived on the current connection.
    pub fn on_announced(&mut self, reason: &str, now: Instant) {
        self.announced = Some(match reason {
            "restart" | "update" => Announced::Restarting { at: now },
            other => Announced::Stopped {
                reason: other.to_owned(),
            },
        });
    }

    /// The authenticated connection was lost.
    pub fn on_lost(&mut self, now: Instant) {
        let stable = self
            .connected_since
            .take()
            .is_some_and(|since| now.saturating_duration_since(since) >= self.config.stable_after);
        if stable {
            self.stable = true;
            self.interval = self.config.min_interval;
        }
    }

    /// The daemon has not answered for the grace period (or a scheduled check is due):
    /// what now. `Start` is recorded as an automatic restart.
    pub fn decide(&mut self, initialized: bool, now: Instant) -> Decision {
        if !initialized {
            return Decision::Uninitialized;
        }
        match &self.announced {
            Some(Announced::Stopped { .. }) => return Decision::StoppedOnPurpose,
            Some(Announced::Restarting { at }) => {
                let until = *at + self.config.comeback;
                if now < until {
                    return Decision::CheckAt(until);
                }
            }
            None => {}
        }
        if let Some(last) = self.last_restart {
            let next = last + self.interval;
            if now < next {
                return Decision::CheckAt(next);
            }
            if !self.stable {
                self.interval = (self.interval * 2).min(self.config.max_interval);
            }
        }
        self.previous_restart = self.last_restart;
        self.last_restart = Some(now);
        self.stable = false;
        self.restarts += 1;
        Decision::Start
    }

    /// An automatic restart decided by [`AutoStartPolicy::decide`] succeeded. When
    /// nothing had to be spawned it does not count.
    pub fn on_restart_succeeded(&mut self, outcome: StartOutcome) {
        if outcome == StartOutcome::AlreadyRunning {
            self.last_restart = self.previous_restart;
            self.restarts = self.restarts.saturating_sub(1);
        }
    }

    /// The initial auto-start failed: further automatic attempts follow the restart
    /// spacing instead of retrying at once.
    pub fn on_initial_start_failed(&mut self, now: Instant) {
        if self.last_restart.is_none() {
            self.last_restart = Some(now);
        }
    }

    /// The user asked for a start (never rate limited): a deliberate stop is forgotten.
    pub fn on_user_start(&mut self) {
        self.announced = None;
    }

    pub fn announced(&self) -> Option<&Announced> {
        self.announced.as_ref()
    }

    /// Reason of a deliberate stop, while it holds.
    pub fn stopped_reason(&self) -> Option<&str> {
        match &self.announced {
            Some(Announced::Stopped { reason }) => Some(reason),
            _ => None,
        }
    }

    /// Automatic restarts done so far.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Current spacing of automatic restarts.
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn policy() -> (AutoStartPolicy, Instant) {
        (
            AutoStartPolicy::new(PolicyConfig::default()),
            Instant::now(),
        )
    }

    #[test]
    fn an_uninitialized_root_is_never_started() {
        let (mut p, t0) = policy();
        assert_eq!(p.on_open(false), Decision::Uninitialized);
        for later in [0, 1, 61, 3600] {
            assert_eq!(p.decide(false, t0 + secs(later)), Decision::Uninitialized);
        }
        assert_eq!(p.restarts(), 0);
    }

    #[test]
    fn an_initialized_root_is_started_on_open() {
        let (mut p, _) = policy();
        assert_eq!(p.on_open(true), Decision::Start);
        // The initial start is not a restart.
        assert_eq!(p.restarts(), 0);
    }

    #[test]
    fn one_restart_after_an_unexpected_loss_then_at_most_one_per_minute() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_lost(t0 + secs(5));
        assert_eq!(p.decide(true, t0 + secs(6)), Decision::Start);
        assert_eq!(p.restarts(), 1);
        p.on_restart_succeeded(StartOutcome::Started);
        // The restarted daemon dies again 10 s later: no second restart within 60 s.
        p.on_connected(t0 + secs(8));
        p.on_lost(t0 + secs(18));
        assert_eq!(
            p.decide(true, t0 + secs(19)),
            Decision::CheckAt(t0 + secs(66))
        );
        assert_eq!(p.restarts(), 1);
        // Due: restart, and the crash loop doubles the spacing.
        assert_eq!(p.decide(true, t0 + secs(66)), Decision::Start);
        assert_eq!(p.interval(), secs(120));
        assert_eq!(p.restarts(), 2);
    }

    #[test]
    fn never_after_a_user_shutdown_until_the_user_starts_it() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_announced("user", t0 + secs(1));
        p.on_lost(t0 + secs(1));
        for later in [2, 61, 3600] {
            assert_eq!(p.decide(true, t0 + secs(later)), Decision::StoppedOnPurpose);
        }
        assert_eq!(p.stopped_reason(), Some("user"));
        assert_eq!(p.restarts(), 0);
        p.on_user_start();
        assert_eq!(p.stopped_reason(), None);
        assert_eq!(p.decide(true, t0 + secs(3601)), Decision::Start);
    }

    #[test]
    fn a_signal_stop_is_deliberate_too() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_announced("signal", t0);
        p.on_lost(t0);
        assert_eq!(p.decide(true, t0 + secs(1)), Decision::StoppedOnPurpose);
    }

    #[test]
    fn a_daemon_restarting_itself_gets_time_to_come_back() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_announced("restart", t0 + secs(1));
        p.on_lost(t0 + secs(1));
        assert_eq!(
            p.decide(true, t0 + secs(2)),
            Decision::CheckAt(t0 + secs(16))
        );
        // It came back: the announcement is forgotten.
        p.on_connected(t0 + secs(4));
        assert_eq!(p.announced(), None);
        // It did not come back: treated as lost.
        let (mut q, t1) = policy();
        q.on_connected(t1);
        q.on_announced("update", t1);
        q.on_lost(t1);
        assert_eq!(q.decide(true, t1 + secs(15)), Decision::Start);
    }

    #[test]
    fn a_stable_daemon_resets_the_spacing() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_lost(t0 + secs(1));
        assert_eq!(p.decide(true, t0 + secs(2)), Decision::Start);
        p.on_lost(t0 + secs(3));
        assert_eq!(p.decide(true, t0 + secs(62)), Decision::Start);
        assert_eq!(p.interval(), secs(120));
        // This one stays up for 5 minutes.
        p.on_connected(t0 + secs(63));
        p.on_lost(t0 + secs(363));
        assert_eq!(p.interval(), secs(60));
        assert_eq!(p.decide(true, t0 + secs(364)), Decision::Start);
        assert_eq!(p.interval(), secs(60));
    }

    #[test]
    fn the_spacing_is_capped() {
        let (mut p, t0) = policy();
        let mut now = t0;
        for _ in 0..12 {
            match p.decide(true, now) {
                Decision::Start => {}
                Decision::CheckAt(at) => {
                    now = at;
                    assert_eq!(p.decide(true, now), Decision::Start);
                }
                other => panic!("{other:?}"),
            }
            p.on_lost(now);
        }
        assert_eq!(p.interval(), secs(600));
    }

    #[test]
    fn a_restart_that_found_a_daemon_running_does_not_count() {
        let (mut p, t0) = policy();
        p.on_connected(t0);
        p.on_lost(t0 + secs(1));
        assert_eq!(p.decide(true, t0 + secs(2)), Decision::Start);
        p.on_restart_succeeded(StartOutcome::AlreadyRunning);
        assert_eq!(p.restarts(), 0);
        p.on_lost(t0 + secs(3));
        assert_eq!(p.decide(true, t0 + secs(4)), Decision::Start);
    }

    #[test]
    fn a_failed_initial_start_waits_for_the_spacing() {
        let (mut p, t0) = policy();
        assert_eq!(p.on_open(true), Decision::Start);
        p.on_initial_start_failed(t0);
        assert_eq!(
            p.decide(true, t0 + secs(1)),
            Decision::CheckAt(t0 + secs(60))
        );
        assert_eq!(p.decide(true, t0 + secs(60)), Decision::Start);
    }
}
