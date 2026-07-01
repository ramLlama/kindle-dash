//! E-ink rendering via `eips`, plus the partial/full refresh cadence.

use super::{Device, sys};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RefreshKind {
    /// Full, flashing refresh — clears e-ink ghosting.
    Full,
    /// Fast partial refresh — no flash, but accumulates ghosting over time.
    Partial,
}

/// Tracks the refresh cadence: after `full_every` partial refreshes, the next one is a
/// full refresh. Mirrors the counter behavior of the original `dash.sh`.
pub struct RefreshCycle {
    full_every: u32,
    count: u32,
}

impl RefreshCycle {
    pub fn new(full_every: u32) -> Self {
        RefreshCycle {
            full_every,
            count: 0,
        }
    }

    /// Decide the next refresh kind and advance the cycle: `full_every` partial
    /// refreshes, then a full one.
    pub fn next_kind(&mut self) -> RefreshKind {
        if self.count >= self.full_every {
            self.count = 0;
            RefreshKind::Full
        } else {
            self.count += 1;
            RefreshKind::Partial
        }
    }

    /// Force the next refresh to be full (used after waking from a long sleep, so the
    /// first post-sleep frame is clean).
    pub fn force_full_next(&mut self) {
        self.count = self.full_every;
    }
}

impl Device {
    /// Render the dashboard image, choosing full vs partial refresh.
    pub fn render_dashboard(self, path: &Path, kind: RefreshKind) {
        self.eips_show(path, kind == RefreshKind::Full);
    }

    /// Render the sleep screen (always a full refresh) and give the panel time to settle.
    pub fn render_sleeping(self, path: &Path) {
        self.render_static(path);
    }

    /// Render the low-battery screen (always a full refresh) and let the panel settle.
    pub fn render_low_battery(self, path: &Path) {
        self.render_static(path);
    }

    /// Full-refresh a static full-screen image and give the panel time to settle before the
    /// device suspends.
    fn render_static(self, path: &Path) {
        self.eips_show(path, true);
        std::thread::sleep(Duration::from_secs(2));
    }

    /// Show an image via the device's `eips` tool. The binary lives at an absolute path
    /// (it isn't on the default PATH) which may differ per model.
    fn eips_show(self, path: &Path, full: bool) {
        let eips = match self {
            Device::Voyage => "/usr/sbin/eips",
        };
        let path = path.to_string_lossy();
        if full {
            sys::run_ok(eips, &["-f", "-g", &path]);
        } else {
            sys::run_ok(eips, &["-g", &path]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_refresh_after_configured_partials() {
        let mut c = RefreshCycle::new(2);
        // 2 partials, then a full, repeating.
        assert_eq!(c.next_kind(), RefreshKind::Partial);
        assert_eq!(c.next_kind(), RefreshKind::Partial);
        assert_eq!(c.next_kind(), RefreshKind::Full);
        assert_eq!(c.next_kind(), RefreshKind::Partial);
        assert_eq!(c.next_kind(), RefreshKind::Partial);
        assert_eq!(c.next_kind(), RefreshKind::Full);
    }

    #[test]
    fn zero_rate_means_every_refresh_is_full() {
        let mut c = RefreshCycle::new(0);
        assert_eq!(c.next_kind(), RefreshKind::Full);
        assert_eq!(c.next_kind(), RefreshKind::Full);
    }

    #[test]
    fn force_full_next_overrides_partial() {
        let mut c = RefreshCycle::new(4);
        assert_eq!(c.next_kind(), RefreshKind::Partial);
        c.force_full_next();
        assert_eq!(c.next_kind(), RefreshKind::Full);
    }
}
