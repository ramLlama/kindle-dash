//! Cron-based wakeup scheduling. Ported from the old standalone `next-wakeup` binary.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// When the next slot is nearer than this, we treat the current frame as *servicing* that
/// slot (we woke a beat early for it) and roll forward to the slot after it. The RTC can
/// resume us a second or so before the alarm, and cron always returns the next fire strictly
/// after `now`; without this roll a wake at 20:59:59 would see the 21:00 slot as a ~1s
/// "sleep", so the current frame would be attributed to the *previous* slot and the real
/// (possibly hours-long) gap would only surface on the next iteration. Cron granularity is
/// one minute, so any threshold between the wake jitter and 60s is safe; the RTC is usually
/// off by only a second, so 5s is a comfortable margin.
const MIN_SLEEP_SECS: i64 = 5;

/// Absolute instant of the next scheduled refresh, in `tz`. Callers derive both the
/// sleep-screen decision and the suspend duration from this single instant, so the two can
/// never straddle a slot boundary (which previously rendered the dashboard yet slept for
/// hours). See [`MIN_SLEEP_SECS`] for the early-wake roll.
pub fn next_refresh_at(schedule: &str, tz: Tz) -> Result<DateTime<Utc>> {
    next_refresh_from(schedule, Utc::now().with_timezone(&tz))
}

/// Core of [`next_refresh_at`], with `now` injected for testability. `now` carries the
/// schedule's timezone (needed to interpret the cron fields), but the result is returned in
/// UTC — we never display local time, only compute durations against it.
fn next_refresh_from<Tz2: chrono::TimeZone>(
    schedule: &str,
    now: DateTime<Tz2>,
) -> Result<DateTime<Utc>> {
    let mut next = cron_parser::parse(schedule, &now)
        .map_err(|e| anyhow!("invalid cron schedule {schedule:?}: {e:?}"))?;
    // Woke essentially at this slot: roll to the one after so the gap reflects the real sleep.
    if next.clone().signed_duration_since(now).num_seconds() < MIN_SLEEP_SECS {
        next = cron_parser::parse(schedule, &next)
            .map_err(|e| anyhow!("invalid cron schedule {schedule:?}: {e:?}"))?;
    }
    Ok(next.with_timezone(&Utc))
}

/// Validate that `schedule` is a parseable cron expression.
pub fn validate(schedule: &str) -> Result<()> {
    let now = Utc::now();
    cron_parser::parse(schedule, &now)
        .map(|_| ())
        .map_err(|e| anyhow!("invalid cron schedule {schedule:?}: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::UTC;

    /// Seconds from `now` until the computed refresh, for terse assertions.
    fn secs_from(schedule: &str, now: DateTime<Tz>) -> i64 {
        next_refresh_from(schedule, now)
            .unwrap()
            .signed_duration_since(now)
            .num_seconds()
    }

    #[test]
    fn every_minute_is_within_the_next_minute() {
        // On-time wake at :30 past → next minute boundary is 30s away (not rolled).
        let now = UTC.with_ymd_and_hms(2026, 1, 1, 12, 0, 30).unwrap();
        assert_eq!(secs_from("* * * * *", now), 30);
    }

    #[test]
    fn early_wake_rolls_to_the_following_slot() {
        // Woke 1s before the top of the hour on an hourly schedule: instead of a ~1s "sleep"
        // to 13:00, roll forward to 14:00 so the gap reflects the real hour-long sleep.
        let now = UTC.with_ymd_and_hms(2026, 1, 1, 12, 59, 59).unwrap();
        assert_eq!(secs_from("0 * * * *", now), 3601);
    }

    #[test]
    fn last_active_slot_yields_the_long_overnight_gap() {
        // */5 during 6-21: waking a beat before the final 21:55 slot must surface the gap to
        // the next morning's 06:00 (the sleep-screen trigger), not a 1s sleep to 21:55.
        let now = UTC.with_ymd_and_hms(2026, 1, 1, 21, 54, 59).unwrap();
        // 21:55:00 -> next day 06:00:00 is 8h5m1s from `now`.
        assert_eq!(secs_from("*/5 6-21 * * *", now), 8 * 3600 + 5 * 60 + 1);
    }

    #[test]
    fn mid_window_slot_is_not_rolled() {
        // Genuine 5-minute gap between active slots stays a 5-minute sleep.
        let now = UTC.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        assert_eq!(secs_from("*/5 6-21 * * *", now), 300);
    }

    #[test]
    fn timezone_is_accepted() {
        let at = next_refresh_at("2,32 8-17 * * MON-FRI", chrono_tz::Europe::Amsterdam).unwrap();
        assert!(at.signed_duration_since(Utc::now()).num_seconds() > 0);
    }

    #[test]
    fn garbage_schedule_is_rejected() {
        assert!(validate("not a cron expression").is_err());
        assert!(next_refresh_at("nope", UTC).is_err());
    }

    #[test]
    fn valid_schedule_passes_validation() {
        assert!(validate("2,32 8-17 * * MON-FRI").is_ok());
    }
}
