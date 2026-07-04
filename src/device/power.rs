//! Power management: framework control, CPU governor, RTC-armed suspend-to-RAM, and
//! wake-source inspection.

use super::{Device, sys};
use anyhow::{Context, Result, bail};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const GOVERNOR_PATH: &str = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor";
const POWER_STATE_PATH: &str = "/sys/power/state";
/// `stop`/`start` are symlinks to initctl in /sbin, which isn't on the default PATH.
const STOP_BIN: &str = "/sbin/stop";
const START_BIN: &str = "/sbin/start";
/// max77696 PMIC interrupt raised by a physical power-button press.
const POWER_BUTTON_IRQ_LABEL: &str = "max77696-onkey_press";

/// Waits shorter than this never suspend to RAM. An RTC alarm armed only a few seconds
/// out can fire while the kernel is still *entering* suspend; the interrupt is then
/// serviced (and the one-shot alarm spent) before the transition completes, and the
/// device sleeps with no wake source left — observed on hardware as an infinite sleep.
/// Staying awake for short waits sidesteps the race entirely. With the abort window
/// below, the alarm is nominally armed ~35s ahead of the actual suspend.
const MIN_SUSPEND_SECS: i64 = 45;
/// Awake window before each real suspend, so a freshly-launched process can be stopped
/// (e.g. `kill` over SSH, or the power button) before the device drops off the network.
const SUSPEND_ABORT_WINDOW_SECS: u64 = 10;
/// Greatest time until the wake instant, once actually entering suspend, that isn't worth
/// suspending for. Sleeping is an *at least* guarantee, so the abort window can oversleep
/// and eat the margin `MIN_SUSPEND_SECS` nominally leaves; the remainder is re-checked
/// right before suspending and anything at or below this is slept off awake instead.
const MIN_ACTUAL_SUSPEND_SECS: i64 = 30;
/// Poll cadence for the shutdown flag and power-button count during awake sleeps.
const POLL_INTERVAL_SECS: u64 = 1;

/// Why a [`Device::suspend_for`] wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// The requested time elapsed (RTC alarm fired, or an awake sleep ran out).
    Timer,
    /// The physical power button was pressed.
    PowerButton,
    /// SIGINT/SIGTERM set the shutdown flag.
    Signal,
}

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

    /// Wait `secs` seconds, suspending to RAM when possible, and report why the wait
    /// ended. This is the single sleep/suspend entrypoint for the main loop.
    ///
    /// - `sleep_only` (debug mode) or a wait shorter than [`MIN_SUSPEND_SECS`]: stay
    ///   awake and sleep it off, honoring the shutdown flag and the power button.
    ///   Non-positive `secs` (a refresh slot that passed while servicing the previous
    ///   frame) returns [`WakeReason::Timer`] immediately so the caller re-plans.
    /// - Otherwise: stay awake for [`SUSPEND_ABORT_WINDOW_SECS`] first, then arm the RTC
    ///   and suspend. The wake instant is fixed at entry, so the window doesn't shift
    ///   the scheduled wakeup. If the window overslept and left at or below
    ///   [`MIN_ACTUAL_SUSPEND_SECS`] until that instant, the rest is slept off awake
    ///   instead of suspending. After resuming, the power-button interrupt count tells a
    ///   button wake from an RTC one (this firmware has no readable wake-reason property).
    pub fn suspend_for(
        self,
        secs: i64,
        sleep_only: bool,
        shutdown: &AtomicBool,
    ) -> Result<WakeReason> {
        let wake_at = chrono::Utc::now().timestamp() + secs;
        let pwr_baseline = self.power_button_irq_count();

        if sleep_only || secs < MIN_SUSPEND_SECS {
            let awake_secs = secs.max(0) as u64;
            if sleep_only {
                log::info!("[debug] would suspend for {secs}s; sleeping awake instead");
            } else {
                log::info!(
                    "sleeping awake for {awake_secs}s (below {MIN_SUSPEND_SECS}s suspend threshold)"
                );
            }
            return Ok(self.interruptible_sleep(awake_secs, shutdown, pwr_baseline));
        }

        match self.interruptible_sleep(SUSPEND_ABORT_WINDOW_SECS, shutdown, pwr_baseline) {
            WakeReason::Timer => {}
            interrupted => return Ok(interrupted),
        }

        // The abort window sleeps *at least* its duration; re-check what's actually left
        // so an oversleep can't shrink the arm-to-suspend margin into the lost-alarm race.
        let remaining = wake_at - chrono::Utc::now().timestamp();
        if remaining <= MIN_ACTUAL_SUSPEND_SECS {
            log::info!(
                "only {remaining}s left after abort window (<= {MIN_ACTUAL_SUSPEND_SECS}s); sleeping awake"
            );
            return Ok(self.interruptible_sleep(remaining.max(0) as u64, shutdown, pwr_baseline));
        }

        log::info!("suspending for {remaining}s");
        self.suspend_to_ram(wake_at)?;

        // Resumed — a strict increase in the button count across the suspend means the
        // button woke us; unknown counts (None) count as a timer wake so the loop keeps
        // running and re-arms the RTC.
        let pwr_after = self.power_button_irq_count();
        if matches!((pwr_baseline, pwr_after), (Some(b), Some(a)) if a > b) {
            return Ok(WakeReason::PowerButton);
        }
        Ok(WakeReason::Timer)
    }

    /// Arm the RTC to wake the device at the absolute epoch second `wake_at`, then
    /// suspend to RAM. Blocks until an interrupt (the RTC alarm or the power button)
    /// resumes the CPU.
    ///
    /// Uses the standard Linux RTC sysfs alarm, clearing any pending alarm first (the
    /// kernel rejects a new alarm while one is set). All of the device's RTC nodes are
    /// armed as a hedge; only one needs to succeed.
    fn suspend_to_ram(self, wake_at: i64) -> Result<()> {
        let wake_at = wake_at.to_string();
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
            .with_context(|| format!("writing 'mem' to {POWER_STATE_PATH}"))
    }

    /// Sleep up to `secs` while staying awake, returning early with the cause if the
    /// shutdown flag gets set or the power-button count rises above `pwr_baseline`.
    /// An unreadable interrupt count (`None`) disables button detection for the sleep.
    fn interruptible_sleep(
        self,
        secs: u64,
        shutdown: &AtomicBool,
        pwr_baseline: Option<u64>,
    ) -> WakeReason {
        let mut remaining = secs;
        loop {
            if shutdown.load(Ordering::SeqCst) {
                return WakeReason::Signal;
            }
            let pwr_now = self.power_button_irq_count();
            if matches!((pwr_baseline, pwr_now), (Some(b), Some(n)) if n > b) {
                return WakeReason::PowerButton;
            }
            if remaining == 0 {
                return WakeReason::Timer;
            }
            let chunk = remaining.min(POLL_INTERVAL_SECS);
            std::thread::sleep(Duration::from_secs(chunk));
            remaining -= chunk;
        }
    }

    /// Current cumulative count of power-button-press interrupts, from `/proc/interrupts`.
    /// This firmware exposes no readable powerd wake-reason property, so we compare this
    /// count across a suspend (or an awake sleep) to detect a button press.
    fn power_button_irq_count(self) -> Option<u64> {
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

    #[test]
    fn non_positive_wait_returns_timer_immediately_without_suspending() {
        // Regression for the infinite-sleep bug: a refresh slot that passed during
        // servicing produced a negative wait, which used to arm a ~1s RTC alarm that was
        // lost during suspend entry. It must instead return promptly so the loop re-plans.
        let shutdown = AtomicBool::new(false);
        let start = std::time::Instant::now();
        let reason = Device::Voyage.suspend_for(-8, false, &shutdown).unwrap();
        assert_eq!(reason, WakeReason::Timer);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn preset_shutdown_flag_interrupts_the_wait() {
        // The flag is checked before the first sleep chunk, so this returns immediately
        // even though a 20s awake sleep was requested.
        let shutdown = AtomicBool::new(true);
        let start = std::time::Instant::now();
        let reason = Device::Voyage.suspend_for(20, true, &shutdown).unwrap();
        assert_eq!(reason, WakeReason::Signal);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
