# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Rewrote the on-device code as a single Rust binary.** All shell scripts (`dash.sh`,
  `start.sh`, `stop.sh`, `wait-for-wifi.sh`, and the `local/` hooks) and the separate
  `next-wakeup` helper are folded into one `kindle-dash` binary.
- **Dropped the bundled `xh` HTTP client.** HTTPS is now handled in-process via rustls
  (ring), removing the ~8.6 MB bundled binary and its build-time git clone.
- **Configuration moved from `local/env.sh` to a `config.toml` file** read next to the
  binary. The image URL replaces the `fetch-dashboard.sh` hook; the low-battery shell
  hook is replaced by an optional rate-limited webhook.
- Refresh cadence is now consistent: exactly `full_display_refresh_rate` partial
  refreshes between full refreshes (the old script did one fewer after the first cycle).

### Added

- **Power-button controls:** a single press wakes the device for an immediate refresh and
  keeps the loop running; three presses in a row break the loop, restart the UI framework,
  and return to the Kindle Home UI. `SIGTERM`/`SIGINT` also exit. Presses are counted via
  the `max77696-onkey_press` count in `/proc/interrupts` (this firmware exposes no
  wake-reason property), with a rolling settle window so a deliberate multi-press registers.
- **Kindle Voyage support**, verified on-device: serial via `com.lab126.system usid`
  (falling back to `/proc/cpuinfo`), battery via `com.lab126.powerd battLevel`, UI via the
  `framework` upstart job, and RTC wake via both `rtc0`/`rtc1` `wakealarm` nodes.
- **Supported-device allowlist:** the binary detects the model from its serial number and
  refuses to run on unverified devices (override with `KINDLE_DEVICE=voyage`).
- Logs are written to a file (configurable) and stdout/stderr are redirected there, so
  output never corrupts the e-ink framebuffer.

### Removed

- The `docs/screenshotter/` Puppeteer reference and all image-production tooling; image
  rendering is fully out of scope for this repo.

## [v1.0.0-beta.4] - 2022-07-27

### Changed

- Only call eips if fetch-dashboard succesfully completes
- Ensure a full screen refresh is triggered after wake from sleep
- Build ht from upstream sources, using rusttls instead of vendored openssl
- Replace ht 0.4.0 with xh 0.16.1 (project was renamed)

## [v1.0.0-beta.3] - 2020-02-03

### Changed

- Use 1.1.1.1 as default Wi-Fi test ip
- Use a more standards-compliant cron parser (BREAKING)

### Added

- Add low battery reporting (`local/low-battery.sh`)
- Add debug mode (DEBUG=true start.sh)
- SSH server prerequisite in docs (@julianlam)

### Fixed

- Typos (@jcmiller11, @starcoat)

## [v1.0.0-beta-2] - 2020-01-26

### Removed

- Power state logging

## [v1.0.0-beta-1] - 2020-01-26

Initial release 🎉
