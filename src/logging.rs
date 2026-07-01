//! Logging via the `log` facade backed by simplelog, writing to a configurable sink.
//! On the Kindle, any console output lands on the e-ink framebuffer and corrupts the
//! dashboard, so logs default to a file and a panic hook keeps panic output off the
//! screen too. Choosing a `stdout`/`stderr` sink is opt-in, for host or over-SSH debugging.

use anyhow::{Context, Result};
use log::LevelFilter;
use simplelog::{ConfigBuilder, SimpleLogger, WriteLogger, format_description};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A destination for log output.
pub enum Sink {
    File(PathBuf),
    /// Discard all output (`/dev/null` equivalent).
    Null,
    Stdout,
    Stderr,
}

impl Sink {
    /// Interpret an optional config value — a path, or one of `null`/`stdout`/`stderr` —
    /// resolving a relative path against `base`. `None` falls back to `default_file`.
    pub fn from_config(value: Option<&str>, base: &Path, default_file: PathBuf) -> Sink {
        match value {
            None => Sink::File(default_file),
            Some("null") => Sink::Null,
            Some("stdout") => Sink::Stdout,
            Some("stderr") => Sink::Stderr,
            Some(p) => {
                let path = Path::new(p);
                let resolved = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    base.join(path)
                };
                Sink::File(resolved)
            }
        }
    }
}

/// Install the global logger. Call exactly once. Timestamps are UTC; `debug` raises the
/// level from Info to Debug.
pub fn init(sink: Sink, debug: bool) -> Result<()> {
    let level = if debug {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let config = {
        let mut b = ConfigBuilder::new();
        // UTC "YYYY-MM-DD HH:MM:SS" (simplelog defaults to UTC when no offset is set).
        b.set_time_format_custom(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        ));
        // Strip the target/thread/source-location columns: we want "<ts> <LEVEL> <msg>".
        b.set_target_level(LevelFilter::Off);
        b.set_thread_level(LevelFilter::Off);
        b.set_location_level(LevelFilter::Off);
        b.build()
    };

    match sink {
        Sink::File(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening log file {}", path.display()))?;
            WriteLogger::init(level, config, file)
        }
        Sink::Null => WriteLogger::init(level, config, std::io::sink()),
        Sink::Stdout => SimpleLogger::init(level, config),
        Sink::Stderr => WriteLogger::init(level, config, std::io::stderr()),
    }
    .context("installing logger")?;
    Ok(())
}

/// Before the logger exists, append panic output to `path` so a panic during startup
/// (e.g. bad config) never lands on the e-ink console. Best-effort.
pub fn install_early_panic_hook(path: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let ts = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "{ts} PANIC {info}");
        }
    }));
}

/// Once the logger is installed, route panics through it (to the configured sink) with a
/// backtrace, replacing the early hook. Keeps panic output off the e-ink console.
pub fn route_panics_to_log() {
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::capture();
        log::error!("panic: {info}\n{bt}");
    }));
}
