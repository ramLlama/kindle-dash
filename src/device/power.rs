//! Power management: framework control, CPU governor, RTC-armed suspend-to-RAM, and
//! wake-source inspection.

use super::{Device, sys};
use anyhow::{Context, Result, bail};

const GOVERNOR_PATH: &str = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor";
const POWER_STATE_PATH: &str = "/sys/power/state";
/// `stop`/`start` are symlinks to initctl in /sbin, which isn't on the default PATH.
const STOP_BIN: &str = "/sbin/stop";
const START_BIN: &str = "/sbin/start";
/// max77696 PMIC interrupt raised by a physical power-button press.
const POWER_BUTTON_IRQ_LABEL: &str = "max77696-onkey_press";

impl Device {
    /// Stop the UI framework so it doesn't fight for the screen or drain battery.
    pub fn stop_framework(self) {
        sys::run_ok(STOP_BIN, &[self.framework_service()]);
    }

    /// Restart the UI framework (used on clean exit).
    pub fn start_framework(self) {
        sys::run_ok(START_BIN, &[self.framework_service()]);
    }

    /// Read the current CPU scaling governor, if available.
    pub fn read_cpu_governor(self) -> Option<String> {
        std::fs::read_to_string(GOVERNOR_PATH)
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Set the CPU scaling governor (best-effort).
    pub fn set_cpu_governor(self, governor: &str) {
        if let Err(e) = std::fs::write(GOVERNOR_PATH, governor) {
            log::warn!("could not set cpu governor to {governor}: {e}");
        }
    }

    /// Toggle powerd's screensaver suppression.
    pub fn prevent_screensaver(self, on: bool) {
        let val = if on { "1" } else { "0" };
        sys::run_ok(
            "lipc-set-prop",
            &["com.lab126.powerd", "preventScreenSaver", val],
        );
    }

    /// Return to the native Kindle Home UI.
    pub fn launch_home(self) {
        sys::run_ok(
            "lipc-set-prop",
            &[
                "com.lab126.appmgrd",
                "start",
                "app://com.lab126.booklet.home",
            ],
        );
    }

    /// Arm the RTC to wake the device in `secs` seconds, then suspend to RAM. Blocks until
    /// an interrupt (the RTC alarm or the power button) resumes the CPU.
    ///
    /// Uses the standard Linux RTC sysfs alarm with an absolute epoch time, clearing any
    /// pending alarm first (the kernel rejects a new alarm while one is set). All of the
    /// device's RTC nodes are armed as a hedge; only one needs to succeed.
    pub fn suspend_for(self, secs: i64) -> Result<()> {
        let secs = secs.max(1);
        let wake_at = (chrono::Utc::now().timestamp() + secs).to_string();
        let mut armed = 0;
        for path in self.rtc_wakealarm_paths() {
            // Clear any pending alarm, then arm. Best-effort per node.
            if std::fs::write(path, "0").is_err() {
                continue;
            }
            match std::fs::write(path, &wake_at) {
                Ok(()) => armed += 1,
                Err(e) => log::warn!("could not arm RTC alarm at {path}: {e}"),
            }
        }
        if armed == 0 {
            bail!("failed to arm any RTC wake alarm");
        }

        std::fs::write(POWER_STATE_PATH, "mem")
            .with_context(|| format!("writing 'mem' to {POWER_STATE_PATH}"))?;
        Ok(())
    }

    /// Current cumulative count of power-button-press interrupts, from `/proc/interrupts`.
    /// This firmware exposes no readable powerd wake-reason property, so we compare this
    /// count across a suspend to tell a power-button wake from an RTC-alarm wake.
    pub fn power_button_irq_count(self) -> Option<u64> {
        let interrupts = std::fs::read_to_string("/proc/interrupts").ok()?;
        parse_irq_count(&interrupts, POWER_BUTTON_IRQ_LABEL)
    }
}

/// Sum the per-CPU counts for the interrupt whose name matches `label` in the contents of
/// `/proc/interrupts`.
fn parse_irq_count(interrupts: &str, label: &str) -> Option<u64> {
    let line = interrupts.lines().find(|l| l.trim_end().ends_with(label))?;
    // After the leading "NN:" the columns are per-CPU counts, then the controller name.
    let total: u64 = line
        .split_whitespace()
        .skip(1)
        .map_while(|tok| tok.parse::<u64>().ok())
        .sum();
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real /proc/interrupts line from a Kindle Voyage (single CPU column).
    const IRQ_LINE: &str = "168:          7  max77696-topsys  max77696-onkey_press";

    #[test]
    fn parses_power_button_irq_count() {
        assert_eq!(parse_irq_count(IRQ_LINE, "max77696-onkey_press"), Some(7));
    }

    #[test]
    fn sums_multi_cpu_counts() {
        let line = "168:   3   4   max77696-topsys  max77696-onkey_press";
        assert_eq!(parse_irq_count(line, "max77696-onkey_press"), Some(7));
    }

    #[test]
    fn missing_label_is_none() {
        assert_eq!(parse_irq_count(IRQ_LINE, "max77696-rtc"), None);
    }
}
