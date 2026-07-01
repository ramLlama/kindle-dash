//! Cron-based wakeup scheduling. Ported from the old standalone `next-wakeup` binary.

use anyhow::{Result, anyhow};
use chrono::Utc;
use chrono_tz::Tz;

/// Seconds from now until the next time `schedule` (a cron expression interpreted in
/// `tz`) fires.
pub fn next_wakeup_secs(schedule: &str, tz: Tz) -> Result<i64> {
    let now = Utc::now().with_timezone(&tz);
    let next = cron_parser::parse(schedule, &now)
        .map_err(|e| anyhow!("invalid cron schedule {schedule:?}: {e:?}"))?;
    Ok((next - now).num_seconds())
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
    use chrono_tz::UTC;

    #[test]
    fn every_minute_is_within_the_next_minute() {
        let secs = next_wakeup_secs("* * * * *", UTC).unwrap();
        assert!((0..=60).contains(&secs), "got {secs}");
    }

    #[test]
    fn top_of_hour_is_within_the_next_hour() {
        let secs = next_wakeup_secs("0 * * * *", UTC).unwrap();
        assert!((1..=3600).contains(&secs), "got {secs}");
    }

    #[test]
    fn timezone_is_accepted() {
        // A concrete IANA zone should parse and produce a positive delay.
        let secs = next_wakeup_secs("2,32 8-17 * * MON-FRI", chrono_tz::Europe::Amsterdam).unwrap();
        assert!(secs > 0);
    }

    #[test]
    fn garbage_schedule_is_rejected() {
        assert!(validate("not a cron expression").is_err());
        assert!(next_wakeup_secs("nope", UTC).is_err());
    }

    #[test]
    fn valid_schedule_passes_validation() {
        assert!(validate("2,32 8-17 * * MON-FRI").is_ok());
    }
}
