# kindle-dash

## What This Project Does

`kindle-dash` turns a jailbroken **Kindle Voyage** into a low-power e-ink dashboard. A single
static Rust binary wakes on a cron schedule, fetches a pre-rendered grayscale PNG (over HTTPS or
from a local `file://` path), draws it to the e-ink panel via the device's `eips` tool, then
suspends the device to RAM until the next scheduled refresh. Pressing the physical **power
button** breaks the loop and returns to the native Kindle Home UI.

This is a from-scratch Rust rewrite of an earlier project that was a collection of POSIX shell
scripts plus a standalone `next-wakeup` helper and a bundled `xh` HTTP client. All of that is
gone: there is now one binary, no shell runtime, no bundled tools.

## Tech Stack

- **Rust** edition 2024, toolchain pinned to **1.96** (`rust-toolchain.toml`).
- Cross-compiled to **`arm-unknown-linux-musleabi`** (static musl) via [`cross`](https://github.com/cross-rs/cross) (needs Docker).
- Key crates: `anyhow`, `serde` + `toml` (config), `chrono` + `chrono-tz` + `cron-parser`
  (scheduling), `ureq` + rustls/`ring` (HTTPS fetch), `ctrlc` (signals), `log` + `simplelog`
  (logging).
- **TLS note:** rustls with the `ring` provider is deliberate. `native-tls` is avoided (Kindle's
  system OpenSSL is too old for modern HTTPS) and `aws-lc-rs` needs a C toolchain that won't
  cross-compile cleanly to musl ARM. `ring` is the same stack the old bundled `xh` used.
- On-device dependencies: the Kindle firmware's `lipc-get-prop`/`lipc-set-prop`, `eips`,
  `/sbin/stop`, `/sbin/start`, and standard Linux sysfs (`/sys/power/state`, RTC wakealarm,
  cpufreq governor, `/proc/interrupts`). No package manager or extra tools required on the device.

## Repository Structure

```
src/
  main.rs        Init → main_loop → RAII cleanup. The only file holding business logic.
  config.rs      TOML config load/validate (serde, deny_unknown_fields).
  fetch.rs       Image fetch: ureq/rustls + file://, retry w/ transient/permanent classification, PNG magic check.
  schedule.rs    Cron → seconds-until-next-wakeup.
  logging.rs     `log` facade over simplelog; configurable sink; panic hooks.
  device/        All model-specific behavior, behind `enum Device` methods.
    mod.rs       `enum Device { Voyage }`, detection (serial allowlist), resolution, RTC/framework config.
    power.rs     framework stop/start, CPU governor, RTC-armed suspend, power-button IRQ counting.
    display.rs   eips rendering + partial/full RefreshCycle cadence.
    battery.rs   battery percentage via powerd.
    sys.rs       centralized shell-out helpers (`run`, `run_ok`) for device tools.
assets/1448x1072/  Bundled full-screen PNGs: sleeping.png, low-battery.png, test-dashboard.png.
KUAL/kindle-dash/  KUAL launcher extension (config.xml, menu.json, run.sh).
Makefile           dist / tarball / test / lint / format / clean.
config.toml.example  Documented config template.
```

## Key Concepts & Domain Model

- **The refresh loop** (`main_loop` in `main.rs`): each iteration reads battery, computes the next
  cron wakeup, then does one of three things, then suspends:
  1. **Low battery** (`<= low_battery_pct`): show `low-battery.png`, skip fetching, sleep a fixed
     `low_battery_sleep_secs`, and re-check (resumes on its own once charging lifts the level).
  2. **Long gap** (next wakeup `> sleep_screen_interval`): show `sleeping.png` instead of fetching.
  3. **Normal**: fetch the dashboard PNG and render it.
- **RefreshCycle** (`display.rs`): tracks partial-vs-full e-ink refreshes. After
  `full_display_refresh_rate` fast partial refreshes, the next is a full (flashing) refresh to
  clear ghosting. Default `0` means every refresh is full (fine at low cadence). The first frame
  after any long sleep is forced full.
- **Wake-source detection**: there is no readable powerd wake-reason property on this firmware.
  The loop snapshots the `max77696-onkey_press` interrupt count from `/proc/interrupts` before
  suspend and compares after resume; a strict increase means the power button woke us (→ exit to
  Home), otherwise it was the RTC alarm (→ keep looping).
- **DeviceRestore RAII guard**: engaging it stops the UI `framework`, sets the CPU governor to
  `powersave`, and suppresses the screensaver. Its `Drop` restores the governor, restarts
  `framework`, waits 5s, and launches Home. See gotcha below on why this must run.

## Architecture Overview

The central design rule: **business logic (`main.rs`) never names a sysfs path, `eips`, or a
`lipc` call directly.** Everything device-specific lives behind methods on `enum Device` in
`src/device/` (`resolution`, `battery_percent`, `render_dashboard`/`render_sleeping`/
`render_low_battery`, `suspend_for`, `power_button_irq_count`, `stop_framework`/`start_framework`,
`read_cpu_governor`/`set_cpu_governor`, `prevent_screensaver`, `launch_home`). `main.rs` speaks
only in these terms. Adding a new Kindle model means adding an `enum` variant and its match arms,
not touching business logic.

**Voyage is the only supported model.** Detection (`device::detect`) reads the serial and matches
it against an allowlist of Voyage device codes; anything else hard-errors *before* any device
state is touched, so the binary never pokes unverified sysfs paths on an unknown model. A
`KINDLE_DEVICE=voyage` env override exists for off-device testing / detection misfires.

**Error/teardown model:** fatal errors `panic!` rather than returning `Err`, so every abnormal
exit unwinds through `DeviceRestore::drop` on a single teardown path. `main` never calls
`process::exit` (which would skip destructors). This is why `Cargo.toml` sets `panic = "unwind"`
in the release profile despite the size cost — see gotchas.

**Logging:** on the Kindle any stdout/stderr output lands on the e-ink framebuffer and corrupts
the dashboard. So logging goes through the `log` facade to a configurable `Sink` (file / null /
stdout / stderr), defaulting to `kindle-dash.log` next to the binary. Two panic hooks keep panic
output off the screen: an early one (before the logger is up, e.g. bad config) that appends to the
default log file, and a post-init one routing panics through `log::error!` with a backtrace.

## Development Workflow

All commands assume Docker is running (for `cross`).

- **Test:** `make test` (`cargo test`) — host-side unit tests, ~28 of them.
- **Lint:** `make lint` (`cargo clippy --all-targets`). CI treats warnings as errors; run
  `cargo clippy --all-targets -- -D warnings` before committing.
- **Format:** `make format` (`cargo fmt`).
- **Build + package:** `make dist` cross-builds the binary and stages the install tree under
  `dist/`. `make tarball` produces `kindle-dash-<version>.tar.zst`. Version is derived as
  `v<Cargo version>-<git sha>[-dirty]`; the Cargo.toml `version` is the single source of truth.

### Cross-compile gotcha (important)

`mise` pins Rust via the `RUSTUP_TOOLCHAIN` env var, which makes `cross` try to use a nonexistent
`<ver>-x86_64` container toolchain and fail. Build from a **clean environment**:

```sh
env -i PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  RUSTUP_TOOLCHAIN=1.96 HOME="$HOME" DOCKER_DEFAULT_PLATFORM=linux/amd64 \
  cross build --release --target arm-unknown-linux-musleabi
```

(`mise` pins the `1.96.0` toolchain while `rust-toolchain.toml` says `1.96` — two distinct rustup
toolchains. If rust-analyzer breaks with "infinite recursion", install its component on the
mise-pinned one: `rustup component add rust-analyzer --toolchain 1.96.0-aarch64-apple-darwin`.)

### Packaging / on-device layout

The tarball unpacks to a single top-level `kindle-dash/` directory containing two
independently-installable trees plus the README:

- `kindle-dash/kindle-dash/` → copy to **`/mnt/us/kindle-dash`** (the app: binary, `assets/`,
  `config.toml`).
- `kindle-dash/KUAL/kindle-dash/` → copy to **`/mnt/us/extensions`** (the KUAL launcher entry).

On the device, copy `config.toml.example` to `config.toml` next to the binary and edit it. Using a
different name for the real config means upgrades (which ship `config.toml.example`) won't clobber
it.

**KUAL launcher:** `menu.json` runs `./run.sh`, which starts the binary via `setsid`. This
`setsid` matters: the binary stops the `framework` UI job to take over the framebuffer, and
without a new session the binary would be killed alongside its KUAL parent process. `config.xml`
uses `dynamic="false"`.

## Critical Idiosyncrasies & Gotchas

- **The device will NOT suspend while USB is physically connected.** `echo mem > /sys/power/state`
  returns `EBUSY` with a cable plugged in. Real suspend can only be validated with the cable
  **unplugged**. SSH access is over USBNetwork (`192.168.15.x`, empty root password) — which means
  you cannot simultaneously SSH in and test a real suspend. Use `DEBUG=true` (env var) to watch a
  fetch/render cycle over SSH *without* suspending (it sleeps instead).
- **Do not switch to `panic = "abort"`.** Teardown depends on unwinding through `DeviceRestore`'s
  `Drop`. Aborting skips destructors and leaves the Kindle's UI `framework` stopped until a reboot.
- **RTC nodes:** suspend arms `/sys/class/rtc/rtc0/wakealarm` *and* `rtc1` with an absolute epoch
  time (both are the max77696 PMIC RTC and are wall-clock synced; `rtc0` is `hctosys`). `rtc2`
  (SoC snvs) is **not** wall-clock synced — do not use it. The code arms both rtc0/rtc1 as a hedge
  because the exact suspend-wake node can't be verified without actually suspending.
- **Never emit to stdout/stderr on-device** — it corrupts the e-ink framebuffer. The `stdout`/
  `stderr` log sinks are for host or over-SSH debugging only.
- **`/tmp` is a RAM-backed tmpfs** (`/var`, 64 MB) that survives suspend-to-RAM; it's the default
  scratch dir for the fetched image, avoiding flash wear.
- **`/sbin/stop` and `/sbin/start`** are used with full paths — `/sbin` is not on the default PATH.

## Hardware-Verified Device Facts (Kindle Voyage)

Validated against a real device. The obvious/expected property names are often wrong here.

- **Serial (DSN):** `lipc-get-prop com.lab126.system usid` (fallback: quoted `Serial` line in
  `/proc/cpuinfo`). `com.lab126.system serialNumber` does **not** exist.
- **Battery:** `lipc-get-prop com.lab126.powerd battLevel`. Not `gasgauge-info`.
- **Suspend:** arm the RTC wakealarm(s), then `echo mem > /sys/power/state`.
- **Wake reason:** diff the `max77696-onkey_press` count in `/proc/interrupts` across the suspend.
  There is no readable powerd wake property.
- **UI service:** the `framework` upstart job (`/sbin/stop framework`, `/sbin/start framework`).
  Stopping `appmgrd` alone does **not** free the framebuffer.
- **Rendering:** `/usr/sbin/eips` (`-g <path>` partial, `-f -g <path>` full/flashing).
- **Return Home:** `lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home`.
- **Resolution string:** `Device::resolution()` returns `"1448x1072"` (used as the assets subdir
  name). The panel is physically 1072×1448 portrait; images must be grayscale PNGs sized for it.

See `~/.claude/.../memory/voyage-lipc-reference.md` for the fuller verified LIPC reference.

## Testing

`cargo test` runs host-side unit tests for the pure logic only: config parse/validate, cron
scheduling, fetch error classification + `file://` handling + PNG validation, `/proc/interrupts`
IRQ parsing, battery string parsing, and the RefreshCycle cadence. The suspend, render (`eips`),
framework-control, and real-fetch-over-Wi-Fi paths **cannot be unit-tested** and must be validated
on hardware (cable unplugged for suspend; `DEBUG=true` over SSH for fetch/render).

## Conventions

- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) (`feat`, `fix`,
  `refactor`, `docs`, `test`, `build`, `ci`, `perf`; no `chore`).
- **Rust:** edition 2024; keep `main.rs` free of device specifics (see Architecture); prefer the
  narrowest visibility (device internals are `pub(super)` / private, only the `Device` API is
  `pub`); `0.x` dependencies are pinned `major.minor` (e.g. `0.11`, `0.10`).
- **Device shell-outs** go through `sys::run` (needs output, errors on failure) or `sys::run_ok`
  (best-effort side effect, logs but doesn't propagate failure) — don't call `Command` directly
  from device methods.
