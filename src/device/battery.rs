//! Battery level reading.

use super::{Device, sys};

impl Device {
    /// Read the battery charge percentage (0..=100), or `None` if it can't be read.
    ///
    /// On the Voyage this comes from powerd's `battLevel` property (verified working;
    /// `gasgauge-info` and `com.lab126.system battLevel` are not used).
    pub fn battery_percent(self) -> Option<u8> {
        match self {
            Device::Voyage => {
                let raw = sys::run("lipc-get-prop", &["com.lab126.powerd", "battLevel"]).ok()?;
                parse_battery(&raw)
            }
        }
    }
}

/// Parse a battery percentage like "83%" or "83" into 0..=100.
fn parse_battery(raw: &str) -> Option<u8> {
    raw.trim().trim_end_matches('%').trim().parse::<u8>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percentage_forms() {
        assert_eq!(parse_battery("83%"), Some(83));
        assert_eq!(parse_battery("7"), Some(7));
        assert_eq!(parse_battery("  100% "), Some(100));
        assert_eq!(parse_battery("0%"), Some(0));
    }

    #[test]
    fn rejects_non_numeric() {
        assert_eq!(parse_battery("garbage"), None);
        assert_eq!(parse_battery(""), None);
        assert_eq!(parse_battery("300"), None); // overflows u8
    }
}
