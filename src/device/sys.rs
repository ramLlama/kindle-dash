//! Centralized helpers for shelling out to Kindle device tools (eips, lipc, ...).

use anyhow::{Context, Result, bail};
use std::process::Command;

/// Run a command and return its trimmed stdout. Errors if it can't be spawned or exits
/// non-zero. Use for calls whose output we need (serial number, battery, wake reasons).
pub(super) fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `{program}`"))?;
    if !output.status.success() {
        bail!(
            "`{program}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a command for its side effect, logging (but not propagating) any failure. Use
/// for best-effort control calls (stopping services, toggling props, rendering).
pub(super) fn run_ok(program: &str, args: &[&str]) {
    match Command::new(program).args(args).output() {
        Ok(o) if o.status.success() => {}
        Ok(o) => log::warn!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => log::warn!("could not run `{program}`: {e}"),
    }
}
