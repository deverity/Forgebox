//! When the next background check of a tool runs (docs/03 §6, docs/05 §4): every
//! `update.check_interval` (6 h by default) with ±jitter, never inside `quiet_hours`
//! (local time) and, with `[updates] metered = "pause"`, not while the connection is
//! metered. `manual` never checks. Pure: the caller passes the clock, the time zone and
//! the metered state (the engine asks the injected probe).

use std::time::Duration;

use forge_core::config::{MeteredPolicy, QuietHours};
use forge_core::error::{ForgeError, Result};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// Why a check was moved later than its interval asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    QuietHours,
    Metered,
}

/// Answer of the scheduler hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextCheck {
    /// `None`: never (policy `manual`).
    pub at: Option<Timestamp>,
    pub deferred: Option<DeferReason>,
}

/// Inputs of [`next_check_at`].
#[derive(Debug, Clone)]
pub struct ScheduleInput<'a> {
    /// Policy `manual`: no background checks.
    pub manual: bool,
    pub interval: Duration,
    pub last_check: Option<Timestamp>,
    pub now: Timestamp,
    /// Fraction of the interval (`0.1` = ±10 %).
    pub jitter: f64,
    /// Seed of the jitter (stable for one tool and one last check).
    pub seed: u64,
    /// Delay of the first check when the tool was never checked.
    pub first_check_delay: Duration,
    pub quiet_hours: Option<&'a QuietHours>,
    pub time_zone: &'a TimeZone,
    pub metered: bool,
    pub metered_policy: MeteredPolicy,
    /// How long a metered connection postpones a check.
    pub metered_retry: Duration,
}

fn add(at: Timestamp, d: Duration) -> Result<Timestamp> {
    let span = jiff::SignedDuration::try_from(d)
        .map_err(|e| ForgeError::InvalidInput(format!("duration {d:?}: {e}")))?;
    at.checked_add(span)
        .map_err(|e| ForgeError::InvalidInput(format!("time overflow: {e}")))
}

/// Deterministic value in `[-1, 1]` from `seed` (splitmix64).
fn unit(seed: u64) -> f64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Seed for a tool and its last check (FNV-1a).
pub fn seed_for(tool: &str, last_check: Option<Timestamp>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let secs = last_check.map_or(0, |t| t.as_second());
    for byte in tool.bytes().chain(secs.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Minute of the day of `at` in `tz`.
fn minute_of_day(at: Timestamp, tz: &TimeZone) -> u16 {
    let local = at.to_zoned(tz.clone());
    (local.hour() as u16) * 60 + local.minute() as u16
}

/// Whether `at` falls in the quiet window.
pub fn in_quiet_hours(quiet: &QuietHours, at: Timestamp, tz: &TimeZone) -> Result<bool> {
    quiet.contains(minute_of_day(at, tz))
}

/// First instant at or after `at` outside the quiet window (minute resolution).
fn after_quiet_hours(quiet: &QuietHours, at: Timestamp, tz: &TimeZone) -> Result<Timestamp> {
    let mut t = at;
    // A window is shorter than a day; walk whole minutes (at most 1440 steps).
    for _ in 0..=1440 {
        if !in_quiet_hours(quiet, t, tz)? {
            return Ok(t);
        }
        let zoned = t.to_zoned(tz.clone());
        let into_minute = Duration::from_secs(u64::from(zoned.second() as u8))
            + Duration::from_nanos(u64::try_from(zoned.subsec_nanosecond()).unwrap_or(0));
        t = add(t, Duration::from_secs(60).saturating_sub(into_minute))?;
    }
    Err(ForgeError::ConfigInvalid(
        "quiet_hours cover the whole day".into(),
    ))
}

/// When the next background check runs (module docs).
pub fn next_check_at(input: &ScheduleInput<'_>) -> Result<NextCheck> {
    if input.manual {
        return Ok(NextCheck {
            at: None,
            deferred: None,
        });
    }
    let base = match input.last_check {
        Some(last) => {
            let jitter = input.jitter.clamp(0.0, 0.5) * unit(input.seed);
            let secs = input.interval.as_secs_f64() * (1.0 + jitter);
            add(last, Duration::from_secs_f64(secs.max(0.0)))?
        }
        None => add(input.now, input.first_check_delay)?,
    };
    let mut at = base.max(input.now);
    let mut deferred = None;
    if input.metered && input.metered_policy == MeteredPolicy::Pause {
        let retry = add(input.now, input.metered_retry)?;
        if retry > at {
            at = retry;
            deferred = Some(DeferReason::Metered);
        }
    }
    if let Some(quiet) = input.quiet_hours {
        let moved = after_quiet_hours(quiet, at, input.time_zone)?;
        if moved != at {
            at = moved;
            deferred = Some(DeferReason::QuietHours);
        }
    }
    Ok(NextCheck {
        at: Some(at),
        deferred,
    })
}

/// Whether a run due `now` must wait (quiet hours first, then a metered connection).
pub fn defer_now(
    quiet: Option<&QuietHours>,
    tz: &TimeZone,
    now: Timestamp,
    metered: bool,
    metered_policy: MeteredPolicy,
) -> Result<Option<DeferReason>> {
    if let Some(quiet) = quiet
        && in_quiet_hours(quiet, now, tz)?
    {
        return Ok(Some(DeferReason::QuietHours));
    }
    if metered && metered_policy == MeteredPolicy::Pause {
        return Ok(Some(DeferReason::Metered));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn input<'a>(now: &str, last: Option<&str>, tz: &'a TimeZone) -> ScheduleInput<'a> {
        ScheduleInput {
            manual: false,
            interval: 6 * HOUR,
            last_check: last.map(ts),
            now: ts(now),
            jitter: 0.0,
            seed: 1,
            first_check_delay: Duration::from_secs(60),
            quiet_hours: None,
            time_zone: tz,
            metered: false,
            metered_policy: MeteredPolicy::Pause,
            metered_retry: Duration::from_secs(1800),
        }
    }

    #[test]
    fn interval_first_check_and_manual() {
        let utc = TimeZone::UTC;
        let i = input("2026-10-27T10:00:00Z", Some("2026-10-27T08:00:00Z"), &utc);
        assert_eq!(
            next_check_at(&i).unwrap().at,
            Some(ts("2026-10-27T14:00:00Z"))
        );
        let overdue = input("2026-10-27T20:00:00Z", Some("2026-10-27T08:00:00Z"), &utc);
        assert_eq!(
            next_check_at(&overdue).unwrap().at,
            Some(ts("2026-10-27T20:00:00Z"))
        );
        let never = input("2026-10-27T10:00:00Z", None, &utc);
        assert_eq!(
            next_check_at(&never).unwrap().at,
            Some(ts("2026-10-27T10:01:00Z"))
        );
        let manual = ScheduleInput {
            manual: true,
            ..input("2026-10-27T10:00:00Z", None, &utc)
        };
        assert_eq!(next_check_at(&manual).unwrap().at, None);
    }

    #[test]
    fn jitter_stays_within_bounds_and_is_deterministic() {
        let utc = TimeZone::UTC;
        let last = ts("2026-10-27T00:00:00Z");
        for seed in 0..200u64 {
            let i = ScheduleInput {
                jitter: 0.1,
                seed,
                ..input("2026-10-27T00:00:00Z", Some("2026-10-27T00:00:00Z"), &utc)
            };
            let at = next_check_at(&i).unwrap().at.unwrap();
            let secs = at.duration_since(last).as_secs();
            assert!((19_440..=23_760).contains(&secs), "seed {seed}: {secs}");
            assert_eq!(next_check_at(&i).unwrap().at.unwrap(), at);
        }
        assert_ne!(seed_for("php", Some(last)), seed_for("nginx", Some(last)));
    }

    #[test]
    fn quiet_hours_postpone_to_the_end_of_the_window() {
        let utc = TimeZone::UTC;
        let quiet = QuietHours {
            start: "22:00".into(),
            end: "07:00".into(),
        };
        // Due at 23:30 → 07:00 next day.
        let i = ScheduleInput {
            quiet_hours: Some(&quiet),
            ..input("2026-10-27T20:00:00Z", Some("2026-10-27T17:30:00Z"), &utc)
        };
        let next = next_check_at(&i).unwrap();
        assert_eq!(next.at, Some(ts("2026-10-28T07:00:00Z")));
        assert_eq!(next.deferred, Some(DeferReason::QuietHours));
        // Due at 14:00: untouched.
        let day = ScheduleInput {
            quiet_hours: Some(&quiet),
            ..input("2026-10-27T10:00:00Z", Some("2026-10-27T08:00:00Z"), &utc)
        };
        assert_eq!(next_check_at(&day).unwrap().deferred, None);
        // Local time zone: 22:00-07:00 in UTC-3 is 01:00-10:00 UTC.
        let brt = TimeZone::fixed(jiff::tz::offset(-3));
        let local = ScheduleInput {
            quiet_hours: Some(&quiet),
            time_zone: &brt,
            ..input("2026-10-27T02:00:00Z", Some("2026-10-26T20:00:00Z"), &utc)
        };
        assert_eq!(
            next_check_at(&local).unwrap().at,
            Some(ts("2026-10-27T10:00:00Z"))
        );
        assert_eq!(
            defer_now(
                Some(&quiet),
                &brt,
                ts("2026-10-27T02:00:00Z"),
                false,
                MeteredPolicy::Pause
            )
            .unwrap(),
            Some(DeferReason::QuietHours)
        );
        // start == end is an empty window.
        let empty = QuietHours {
            start: "00:00".into(),
            end: "00:00".into(),
        };
        assert!(!in_quiet_hours(&empty, ts("2026-10-27T02:00:00Z"), &utc).unwrap());
    }

    #[test]
    fn metered_connection_postpones_unless_allowed() {
        let utc = TimeZone::UTC;
        let i = ScheduleInput {
            metered: true,
            ..input("2026-10-27T20:00:00Z", Some("2026-10-27T08:00:00Z"), &utc)
        };
        let next = next_check_at(&i).unwrap();
        assert_eq!(next.at, Some(ts("2026-10-27T20:30:00Z")));
        assert_eq!(next.deferred, Some(DeferReason::Metered));
        let allowed = ScheduleInput {
            metered_policy: MeteredPolicy::Allow,
            ..i.clone()
        };
        assert_eq!(next_check_at(&allowed).unwrap().deferred, None);
        assert_eq!(
            defer_now(
                None,
                &utc,
                ts("2026-10-27T20:00:00Z"),
                true,
                MeteredPolicy::Pause
            )
            .unwrap(),
            Some(DeferReason::Metered)
        );
        assert_eq!(
            defer_now(
                None,
                &utc,
                ts("2026-10-27T20:00:00Z"),
                true,
                MeteredPolicy::Allow
            )
            .unwrap(),
            None
        );
    }
}
