//! Runtime configuration, loaded from a TOML file (replaces the old `local/env.sh`).

use crate::schedule;
use anyhow::{Context, Result, anyhow, bail};
use chrono_tz::Tz;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// URL of the dashboard PNG to fetch over HTTP(S).
    pub image_url: String,
    /// Cron expression (interpreted in `timezone`) controlling refresh times.
    pub refresh_schedule: String,
    /// IANA timezone name used to interpret `refresh_schedule`.
    pub timezone: String,

    /// Do a full (flashing) e-ink refresh after this many partial refreshes, to clear
    /// ghosting. Defaults to 0, i.e. every refresh is a full one — fine at our low refresh
    /// rate, and it avoids partial-refresh ghosting entirely.
    #[serde(default = "default_full_refresh_rate")]
    pub full_display_refresh_rate: u32,
    /// When the next wakeup is at least this many seconds away, show the sleep screen
    /// instead of fetching a fresh dashboard.
    #[serde(default = "default_sleep_screen_interval")]
    pub sleep_screen_interval: i64,
    /// How long to keep retrying the image fetch (waiting for Wi-Fi) before giving up.
    #[serde(default = "default_wifi_timeout")]
    pub wifi_timeout_secs: u64,
    /// Delay before each suspend, leaving a window to abort a freshly-launched process.
    #[serde(default = "default_pre_suspend_delay")]
    pub pre_suspend_delay_secs: u64,
    /// Battery percentage at or below which the dashboard stops refreshing and shows a
    /// low-battery screen, to preserve charge.
    #[serde(default = "default_low_battery_pct")]
    pub low_battery_pct: u8,
    /// How long to suspend between battery re-checks while in low-battery mode, so the
    /// dashboard resumes on its own once charging lifts the level back above the threshold.
    #[serde(default = "default_low_battery_sleep_secs")]
    pub low_battery_sleep_secs: i64,
    /// Log destination: a file path, or one of `null`, `stdout`, `stderr`. Defaults to
    /// `kindle-dash.log` next to the binary. On the Kindle, `stdout`/`stderr` corrupt the
    /// e-ink framebuffer, so they are only for host or over-SSH debugging.
    #[serde(default)]
    pub log: Option<String>,

    /// Directory holding the bundled assets (e.g. `sleeping.png`). Defaults to
    /// `assets/<resolution>` next to the binary, where `<resolution>` is the running
    /// device's display size.
    #[serde(default)]
    pub assets_dir: Option<PathBuf>,
    /// Scratch directory for the fetched dashboard image. Defaults to `/tmp` (a RAM-backed
    /// tmpfs on the Kindle, so it survives suspend-to-RAM and doesn't wear flash).
    #[serde(default)]
    pub scratch_dir: Option<PathBuf>,
}

fn default_full_refresh_rate() -> u32 {
    0
}
fn default_sleep_screen_interval() -> i64 {
    3600
}
fn default_wifi_timeout() -> u64 {
    30
}
fn default_pre_suspend_delay() -> u64 {
    10
}
fn default_low_battery_pct() -> u8 {
    10
}
fn default_low_battery_sleep_secs() -> i64 {
    3600
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Config::from_toml(&text)
    }

    /// Parse and validate config from TOML text (split out for testing).
    pub fn from_toml(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text).context("parsing config TOML")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.image_url.trim().is_empty() {
            bail!("config: image_url must not be empty");
        }
        self.tz()?;
        schedule::validate(&self.refresh_schedule).context("config: invalid refresh_schedule")?;
        if self.sleep_screen_interval < 0 {
            bail!("config: sleep_screen_interval must be >= 0");
        }
        if self.low_battery_pct > 100 {
            bail!("config: low_battery_pct must be 0..=100");
        }
        if self.low_battery_sleep_secs < 1 {
            bail!("config: low_battery_sleep_secs must be >= 1");
        }
        Ok(())
    }

    /// Parse the configured timezone into a `chrono_tz::Tz`.
    pub fn tz(&self) -> Result<Tz> {
        self.timezone
            .parse::<Tz>()
            .map_err(|e| anyhow!("config: invalid timezone {:?}: {e}", self.timezone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        image_url = "https://example.com/dash.png"
        refresh_schedule = "2,32 8-17 * * MON-FRI"
        timezone = "Europe/Amsterdam"
    "#;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let c = Config::from_toml(MINIMAL).unwrap();
        assert_eq!(c.image_url, "https://example.com/dash.png");
        assert_eq!(c.full_display_refresh_rate, 0);
        assert_eq!(c.sleep_screen_interval, 3600);
        assert_eq!(c.wifi_timeout_secs, 30);
        assert_eq!(c.low_battery_pct, 10);
        assert_eq!(c.low_battery_sleep_secs, 3600);
    }

    #[test]
    fn rejects_missing_required_field() {
        let text = r#"refresh_schedule = "* * * * *"
                      timezone = "UTC""#;
        assert!(Config::from_toml(text).is_err());
    }

    #[test]
    fn rejects_invalid_timezone() {
        let text = r#"image_url = "https://x/y.png"
                      refresh_schedule = "* * * * *"
                      timezone = "Mars/Phobos""#;
        assert!(Config::from_toml(text).is_err());
    }

    #[test]
    fn rejects_invalid_schedule() {
        let text = r#"image_url = "https://x/y.png"
                      refresh_schedule = "not a cron"
                      timezone = "UTC""#;
        assert!(Config::from_toml(text).is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        let text = format!("{MINIMAL}\nbogus_key = 1\n");
        assert!(Config::from_toml(&text).is_err());
    }

    #[test]
    fn rejects_negative_sleep_screen_interval() {
        let text = format!("{MINIMAL}\nsleep_screen_interval = -1\n");
        assert!(Config::from_toml(&text).is_err());
    }

    #[test]
    fn rejects_out_of_range_low_battery_pct() {
        let text = format!("{MINIMAL}\nlow_battery_pct = 150\n");
        assert!(Config::from_toml(&text).is_err());
    }

    #[test]
    fn rejects_non_positive_low_battery_sleep() {
        let text = format!("{MINIMAL}\nlow_battery_sleep_secs = 0\n");
        assert!(Config::from_toml(&text).is_err());
    }
}
