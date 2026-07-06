//! Low-power Kindle Voyage dashboard: fetch a PNG over HTTPS, render it to the e-ink
//! display via `eips`, and suspend to RAM between refreshes. A single power-button press
//! wakes the device for an immediate refresh; pressing it several times in a row breaks
//! the loop and returns to the native Kindle Home UI.

mod config;
mod device;
mod fetch;
mod logging;
mod schedule;

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use config::Config;
use device::{Device, RefreshCycle, WakeReason};

/// Why the main loop stopped.
enum ExitReason {
    PowerButton,
    Signal,
}

fn main() {
    run();
}

/// Fatal errors here `panic!` rather than returning, so every abnormal exit unwinds
/// through `DeviceRestore::drop` (restoring the device) on the one teardown path. Panic
/// output goes to the redirected stderr, i.e. the log file, never the screen. `main`
/// never calls `process::exit`, which would skip destructors.
fn run() {
    let debug = std::env::var("DEBUG").map(|v| v == "true").unwrap_or(false);
    let base = base_dir().expect("resolving base directory");
    let default_log = base.join("kindle-dash.log");

    // Until the real logger is up, keep any startup panic (e.g. bad config) off the
    // e-ink console by appending it to the default log file.
    logging::install_early_panic_hook(default_log.clone());

    let config_path = std::env::var("KINDLE_DASH_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base.join("config.toml"));
    let config = Config::load(&config_path)
        .unwrap_or_else(|e| panic!("loading config from {}: {e:#}", config_path.display()));
    let tz = config.tz().expect("parsing timezone");

    // Install the logger at the configured sink, then route panics through it so nothing
    // ever reaches stdout/stderr (which on the Kindle would corrupt the framebuffer).
    let sink = logging::Sink::from_config(config.log.as_deref(), &base, default_log);
    logging::init(sink, debug).expect("initializing logging");
    logging::route_panics_to_log();

    let device = device::detect().unwrap_or_else(|e| panic!("{e:#}"));
    log::info!(
        "kindle-dash starting (device={device:?}, schedule={:?}, debug={debug})",
        config.refresh_schedule
    );

    // Resolve asset and scratch directories. Config may override either; otherwise assets
    // default to `assets/<resolution>` next to the binary (resolution comes from the
    // detected device) and scratch defaults to `/tmp` (RAM-backed tmpfs on the Kindle).
    let assets_dir = config
        .assets_dir
        .clone()
        .unwrap_or_else(|| base.join("assets").join(device.resolution()));
    let scratch_dir = config
        .scratch_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&scratch_dir)
        .unwrap_or_else(|e| panic!("creating scratch dir {}: {e:#}", scratch_dir.display()));
    let dash_png = scratch_dir.join("dash.png");
    let sleeping_png = assets_dir.join("sleeping.png");
    let low_battery_png = assets_dir.join("low-battery.png");

    // Lock down the UI and power settings, and register cleanup in one step. From here on
    // any exit path — normal return or panic unwinding — runs `DeviceRestore::drop`, so
    // the Kindle is never left with its UI stopped until a reboot. Detection ran *before*
    // this, so an unsupported device exits without ever touching device state.
    let _restore = DeviceRestore::engage(device);

    // Graceful shutdown on SIGINT/SIGTERM.
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        // `set_handler` needs a `'static` closure (it fires on a separate thread later), so
        // the closure can't borrow the local `s`; `move` gives it ownership of the clone.
        let s = shutdown.clone();
        ctrlc::set_handler(move || s.store(true, Ordering::SeqCst))
            .expect("installing signal handler");
    }

    let mut cycle = RefreshCycle::new(config.full_display_refresh_rate);
    let reason = main_loop(
        &config,
        device,
        tz,
        &dash_png,
        &sleeping_png,
        &low_battery_png,
        &mut cycle,
        &shutdown,
        debug,
    );

    match reason {
        ExitReason::PowerButton => log::info!("exiting: power button pressed"),
        ExitReason::Signal => log::info!("exiting: received termination signal"),
    }
    // `_restore` drops here (or during panic unwinding), running device cleanup.
}

/// RAII guard: acquiring it locks down the device for dashboard operation, and dropping
/// it restores normal operation — on normal return *and* on panic unwinding.
struct DeviceRestore {
    device: Device,
    governor: Option<String>,
}

impl DeviceRestore {
    /// Stop the UI framework, switch the CPU to `powersave`, and suppress the
    /// screensaver, capturing the prior governor so `drop` can restore it.
    fn engage(device: Device) -> Self {
        let governor = device.read_cpu_governor();
        device.stop_framework();
        device.set_cpu_governor("powersave");
        device.prevent_screensaver(true);
        DeviceRestore { device, governor }
    }
}

impl Drop for DeviceRestore {
    fn drop(&mut self) {
        log::info!("cleanup: restoring device");
        self.device.prevent_screensaver(false);
        match &self.governor {
            Some(g) => self.device.set_cpu_governor(g),
            // We couldn't read the original governor at startup, so it's left at the
            // `powersave` we set in `engage`. Surface it rather than silently degrade.
            None => log::warn!("original cpu governor unknown; leaving it at powersave"),
        }
        self.device.start_framework();
        // Give the framework a moment to come up before asking it to show Home.
        std::thread::sleep(Duration::from_secs(5));
        self.device.launch_home();
    }
}

#[allow(clippy::too_many_arguments)]
fn main_loop(
    config: &Config,
    device: Device,
    tz: chrono_tz::Tz,
    dash_png: &Path,
    sleeping_png: &Path,
    low_battery_png: &Path,
    cycle: &mut RefreshCycle,
    shutdown: &AtomicBool,
    debug: bool,
) -> ExitReason {
    // Index into `config.image_urls`. A single power-button press advances it (wrapping),
    // so successive presses cycle through the configured dashboards; a timer wake keeps the
    // current one. With a single URL this stays at 0 and the press just refreshes.
    let mut image_idx = 0usize;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return ExitReason::Signal;
        }

        let battery = device.battery_percent();
        if let Some(level) = battery {
            log::info!("battery: {level}%");
        }
        let low_battery = matches!(battery, Some(level) if level <= config.low_battery_pct);

        // Absolute instant of the next scheduled refresh. Servicing a frame (fetch +
        // render) takes many seconds, which can cross a slot boundary, so we pin the
        // target once here and have both the sleep-screen decision below and the sleep
        // duration further down derive from it. The sleep recomputes only the *remaining*
        // time to this fixed instant, which naturally subtracts the servicing time.
        let target = schedule::next_refresh_at(&config.refresh_schedule, tz)
            .unwrap_or_else(|e| panic!("computing next wakeup: {e:#}"));
        let next_secs = target.signed_duration_since(Utc::now()).num_seconds();

        if low_battery {
            // Low battery overrides the schedule: stop fetching/refreshing to preserve charge,
            // show the low-battery screen, and re-check after a long sleep (resumes normal
            // operation on its own once charging lifts the level back above the threshold).
            log::warn!(
                "battery low (<= {}%): showing low-battery screen, pausing refresh",
                config.low_battery_pct
            );
            device.render_low_battery(low_battery_png);
            cycle.force_full_next();
        } else if next_secs > config.sleep_screen_interval {
            log::info!(
                "next wakeup in {next_secs}s (> {}s): showing sleep screen",
                config.sleep_screen_interval
            );
            device.render_sleeping(sleeping_png);
            // Ensure the first frame after a long sleep is a clean full refresh.
            cycle.force_full_next();
        } else {
            let image_url = &config.image_urls[image_idx];
            log::info!(
                "refreshing dashboard {}/{} (next wakeup in {next_secs}s): {image_url}",
                image_idx + 1,
                config.image_urls.len()
            );
            match fetch::fetch_to(image_url, dash_png, config.wifi_timeout_secs) {
                Ok(()) => device.render_dashboard(dash_png, cycle.next_kind()),
                Err(e) => log::warn!("not updating screen: {e:#}"),
            }
        }

        // In low-battery mode sleep a fixed long interval before re-checking; otherwise
        // sleep until the same `target` instant the render decision used, so the seconds
        // spent fetching and rendering are subtracted from the sleep. `suspend_for` owns
        // the whole wait: awake sleep vs real suspend, the pre-suspend abort window, and
        // wake-source detection.
        let sleep_secs = if low_battery {
            config.low_battery_sleep_secs
        } else {
            target.signed_duration_since(Utc::now()).num_seconds()
        };
        match device.suspend_for(sleep_secs, debug, shutdown) {
            Ok(WakeReason::Timer) => log::info!("woke via timer"),
            // A single press cycles to the next configured image; loop around and render it
            // (wraps back to the first, so a single-URL config just refreshes in place).
            Ok(WakeReason::PowerButtonCycleImage) => {
                image_idx = (image_idx + 1) % config.image_urls.len();
                log::info!("woke via power button: cycling to image {}", image_idx + 1);
            }
            Ok(WakeReason::PowerButtonExit) => {
                log::info!("woke via power button: exiting");
                return ExitReason::PowerButton;
            }
            Ok(WakeReason::Signal) => return ExitReason::Signal,
            Err(e) => panic!("suspend failed: {e:#}"),
        }
    }
}

/// Directory the binary lives in — the anchor for config, assets, and logs.
fn base_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving current executable path")?;
    let dir = exe.parent().context("executable has no parent directory")?;
    Ok(dir.to_path_buf())
}
