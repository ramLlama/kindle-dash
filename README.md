# Low-power Kindle Voyage dashboard

Turns a jailbroken Kindle Voyage into an energy-efficient wall dashboard.

## What this is

A single self-contained Rust binary that runs on the Kindle. It periodically fetches a
dashboard image over HTTP(S), renders it to the e-ink display, and suspends the device to
RAM (very power efficient) until the next scheduled update. Pressing the physical **power
button** breaks the loop and returns you to the normal Kindle Home UI.

This code does **not** render the dashboard itself. Produce the image however you like
(any tool that can output a grayscale PNG) and serve it over HTTP(S). Rendering elsewhere
is both more power efficient and more flexible.

Everything is one binary: HTTPS fetching (via a modern, statically-linked TLS stack),
cron scheduling, e-ink rendering, power management, and device detection. There are no
shell scripts and no separate HTTP client to bundle.

## Supported devices

**Kindle Voyage only.** The binary detects the device from its serial number and refuses
to run on anything else, so it never writes to power/RTC paths it hasn't been verified
against. (Other models could be added to the allowlist in `src/device/`.)

## Prerequisites

- A jailbroken Kindle Voyage, with Wi-Fi configured.
- An HTTP(S) endpoint serving a grayscale PNG sized for the Voyage's display (or a local
  PNG referenced with a `file://` URL, handy for testing).
- [for debugging] - An SSH server on the Kindle (via [USBNetwork](https://wiki.mobileread.com/wiki/USBNetwork)).

## Installation

A release (or your own `make dist` / `make tarball`) unpacks into a single `kindle-dash/`
folder holding two install trees:

- `kindle-dash/kindle-dash/` is the app: the binary, `config.toml.example`, and assets.
  It installs to `/mnt/us/kindle-dash`.
- `kindle-dash/KUAL/` is the KUAL launcher entry. It installs to `/mnt/us/extensions`.

**N.B: In all instructions, replace `/kindle-mount/` with the path to where your kindle drive is mounted.**

1. Download the latest release and extract it, **or** build it yourself (see below).
2. Copy the two trees onto the Kindle.
   ```sh
   cp -r dist/kindle-dash/kindle-dash/ /kindle-mount/kindle-dash
   cp -r dist/kindle-dash/KUAL/ /kindle-mount/extensions
   ```
3. Create your config from the example and edit it:
   `cp /kindle-mount/kindle-dash/config.toml.example /kindle-mount/kindle-dash/config.toml`
4. Launch it from KUAL: open KUAL and tap **Kindle Dashboard**.

To stop: **press the power button** (returns to Home), or `kill` the process over SSH.

> **Note:** the Kindle will not suspend while a USB cable is connected. The write to
> `/sys/power/state` returns `EBUSY` until the cable is unplugged, so real suspend only
> happens on battery. This only affects tethered (SSH/charging) debugging sessions.

### KUAL launcher

The KUAL entry runs `./run.sh`, which starts the binary detached with `setsid`. That
detachment matters: the dashboard stops the Kindle UI `framework` job to take over the
screen, and without a new session the KUAL-launched process would be torn down with it.

## Configuration

All settings live in `config.toml` next to the binary. See
[`config.toml.example`](./config.toml.example) for the full list: image source, cron
`refresh_schedule`, `timezone`, refresh cadence, sleep-screen threshold, low-battery
threshold, Wi-Fi timeout, and log destination.

## How it works

1. On start it detects the device, stops the Kindle UI framework, sets the CPU governor
   to `powersave`, and prevents the screensaver.
2. Each cycle it reads the battery and computes the next wakeup from the cron schedule. At
   or below `low_battery_pct` it shows a low-battery screen and pauses refreshing. If the
   next wakeup is far off (`sleep_screen_interval`) it shows a sleep screen. Otherwise it
   fetches the image and renders it, alternating fast partial refreshes with periodic full
   refreshes to clear e-ink ghosting.
3. It arms the RTC wake alarm and suspends to RAM (`/sys/power/state`).
4. On wake it checks the power-button interrupt count in `/proc/interrupts`. An RTC wake
   loops again. A **power-button** wake exits cleanly, restoring the framework and
   returning Home.

All logging goes through a configurable sink (a file by default), and a panic hook keeps
panic output off the console too, so nothing is ever left on stdout/stderr, which on the
Kindle would corrupt the e-ink framebuffer. A `stdout`/`stderr` log sink can be selected
for host or over-SSH debugging.

## Building

Requires Docker (for [`cross`](https://github.com/cross-rs/cross)) to cross-compile for
the Kindle's `arm-unknown-linux-musleabi` target.

```sh
make dist      # cross-compile + assemble ./dist
make tarball   # + produce a release tarball
make test      # host-side unit tests
make lint      # cargo clippy
```

## Credits

This project began as a fork of
[pascalw/kindle-dash](https://github.com/pascalw/kindle-dash) and owes it the original
concept and approach. It has since been rewritten from the ground up into a single Rust
binary and grown into its own thing.
