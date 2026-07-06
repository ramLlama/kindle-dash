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
/// After the first power-button press is seen, keep watching this long for more presses
/// before deciding refresh-vs-exit. The waking press is counted before the CPU is fully
/// up, so this window is what lets a deliberate double-press register as two.
const WAKE_SETTLE_SECS: u64 = 2;
/// Number of power-button presses that means "exit to Home". Fewer than this (i.e. a
/// single press) means "refresh now" instead.
const EXIT_PRESS_COUNT: u64 = 3;

/// Why a [`Device::suspend_for`] wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// The requested time elapsed (RTC alarm fired, or an awake sleep ran out).
    Timer,
    /// A single power-button press: wake and re-render now, but keep the loop running.
    /// Treated like [`WakeReason::Timer`] by the loop, but distinct so the pre-suspend
    /// abort window can tell "user wants a refresh" from "the window simply elapsed".
    PowerButtonRefresh,
    /// The power button was pressed at least [`EXIT_PRESS_COUNT`] times: exit to Home.
    PowerButtonExit,
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
    /// - Otherwise: stay awake for [`SUSPEND_ABORT_WINDOW_SECS`] first, then hand off to
    ///   [`Device::suspend_to_ram`], which makes the final suspend-vs-sleep decision as
    ///   late as possible (after clearing the RTC) and arms + suspends. The wake instant
    ///   is fixed at entry, so the window doesn't shift the scheduled wakeup. After
    ///   resuming, the power-button interrupt count tells a button wake from an RTC one
    ///   (this firmware has no readable wake-reason property); a lone press reports
    ///   [`WakeReason::PowerButtonRefresh`] and [`EXIT_PRESS_COUNT`] or more reports
    ///   [`WakeReason::PowerButtonExit`].
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

        // Awake abort window: a press here (PowerButtonRefresh/Exit) or a signal aborts
        // the suspend and bubbles up; only a plain timeout falls through to suspend.
        match self.interruptible_sleep(SUSPEND_ABORT_WINDOW_SECS, shutdown, pwr_baseline) {
            WakeReason::Timer => {}
            interrupted => return Ok(interrupted),
        }

        self.suspend_to_ram(wake_at, shutdown, pwr_baseline)
    }

    /// Make the final suspend-vs-sleep decision and carry it out, returning why the wait
    /// ended.
    ///
    /// The RTC nodes are cleared and armed *first*, then the suspend-vs-sleep check runs as
    /// the very last thing before `echo mem`, so nothing (RTC I/O, logging) sits between it
    /// and the point of no return: an oversleeping abort window can't shrink the margin into
    /// the lost-alarm race. Arming before the check is safe because an alarm firing while
    /// still awake is harmless — only *entering* suspend past the alarm instant is the race.
    /// If at or below [`MIN_ACTUAL_SUSPEND_SECS`] remains, the rest is slept off awake (the
    /// armed alarms fire harmlessly and are re-cleared next iteration); otherwise the device
    /// suspends until the RTC alarm or the power button resumes the CPU, after which the
    /// button count is classified.
    fn suspend_to_ram(
        self,
        wake_at: i64,
        shutdown: &AtomicBool,
        pwr_baseline: Option<u64>,
    ) -> Result<WakeReason> {
        // Clear any pending alarm on every node (the kernel rejects a new alarm while one
        // is set), then arm all nodes as a hedge (only one need succeed) with the fixed
        // wake instant. Best-effort per node.
        let wake_at_str = wake_at.to_string();
        let mut armed = 0;
        for path in self.rtc_wakealarm_paths() {
            if std::fs::write(path, "0").is_err() {
                continue;
            }
            match std::fs::write(path, &wake_at_str) {
                Ok(()) => armed += 1,
                Err(e) => log::warn!("could not arm RTC alarm at {path}: {e}"),
            }
        }
        if armed == 0 {
            bail!("failed to arm any RTC wake alarm");
        }

        // Log the intended suspend *before* the final time reading, so its flush latency
        // lands here and never between that reading and `echo mem`.
        log::info!(
            "suspending for ~{}s",
            wake_at - chrono::Utc::now().timestamp()
        );

        // Final go/no-go, as the very last thing before the point of no return: with the
        // alarms already armed, nothing (RTC I/O, logging) sits between this check and
        // `echo mem`. An alarm that fires while we're still awake here is harmless; the race
        // to avoid is *entering* suspend after the alarm instant, where the one-shot fires
        // during suspend entry and leaves no wake source. If too little time is left, sleep
        // it off awake instead — the armed alarms just fire harmlessly and are re-cleared
        // next iteration.
        let remaining = wake_at - chrono::Utc::now().timestamp();
        if remaining <= MIN_ACTUAL_SUSPEND_SECS {
            log::info!(
                "only {remaining}s left before suspending (<= {MIN_ACTUAL_SUSPEND_SECS}s); sleeping awake"
            );
            return Ok(self.interruptible_sleep(remaining.max(0) as u64, shutdown, pwr_baseline));
        }
        std::fs::write(POWER_STATE_PATH, "mem")
            .with_context(|| format!("writing 'mem' to {POWER_STATE_PATH}"))?;

        // Resumed — a button press since baseline means the button woke us; classify a
        // single vs multi press. Unknown counts (None) count as a timer wake so the loop
        // keeps running and re-arms the RTC.
        if button_pressed(pwr_baseline, self.power_button_irq_count()) {
            Ok(self.classify_button_wake(shutdown, pwr_baseline))
        } else {
            Ok(WakeReason::Timer)
        }
    }

    /// Sleep up to `secs` while staying awake, returning early with the cause if the
    /// shutdown flag gets set or the power-button count rises above `pwr_baseline`.
    /// A press hands off to [`Device::classify_button_wake`] to tell refresh from exit.
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
            if button_pressed(pwr_baseline, self.power_button_irq_count()) {
                return self.classify_button_wake(shutdown, pwr_baseline);
            }
            if remaining == 0 {
                return WakeReason::Timer;
            }
            let chunk = remaining.min(POLL_INTERVAL_SECS);
            std::thread::sleep(Duration::from_secs(chunk));
            remaining -= chunk;
        }
    }

    /// Called once at least one power-button press is known, this watches for further
    /// presses and reports [`WakeReason::PowerButtonExit`] once the count reaches
    /// [`EXIT_PRESS_COUNT`], or [`WakeReason::PowerButtonRefresh`] for a lone press.
    ///
    /// The window is a rolling [`WAKE_SETTLE_SECS`] of quiet: each additional press resets
    /// it, so a deliberate multi-press has time to land rather than needing every press
    /// inside one fixed window. A shutdown signal during the wait wins.
    fn classify_button_wake(self, shutdown: &AtomicBool, pwr_baseline: Option<u64>) -> WakeReason {
        // Without a baseline we can't count deltas; treat the wake as a single press.
        let Some(base) = pwr_baseline else {
            return WakeReason::PowerButtonRefresh;
        };
        let mut last_delta = 0u64;
        let mut quiet = WAKE_SETTLE_SECS;
        loop {
            if shutdown.load(Ordering::SeqCst) {
                return WakeReason::Signal;
            }
            let delta = self
                .power_button_irq_count()
                .map_or(last_delta, |n| n.saturating_sub(base));
            if delta >= EXIT_PRESS_COUNT {
                return WakeReason::PowerButtonExit;
            }
            if delta > last_delta {
                // A new press landed; give it another full window of quiet.
                last_delta = delta;
                quiet = WAKE_SETTLE_SECS;
            }
            if quiet == 0 {
                return WakeReason::PowerButtonRefresh;
            }
            let chunk = quiet.min(POLL_INTERVAL_SECS);
            std::thread::sleep(Duration::from_secs(chunk));
            quiet -= chunk;
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

/// Whether the power button was pressed since `baseline`: a strictly higher current count.
/// An unreadable count on either side (`None`) means "no press detected" — the single
/// definition of a press, shared by the resume and awake-sleep paths.
fn button_pressed(baseline: Option<u64>, now: Option<u64>) -> bool {
    matches!((baseline, now), (Some(b), Some(n)) if n > b)
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
    fn button_pressed_only_on_strict_increase_with_known_counts() {
        assert!(button_pressed(Some(3), Some(4)));
        assert!(!button_pressed(Some(3), Some(3)));
        assert!(!button_pressed(Some(4), Some(3)));
        // An unreadable count on either side never counts as a press.
        assert!(!button_pressed(None, Some(4)));
        assert!(!button_pressed(Some(3), None));
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
