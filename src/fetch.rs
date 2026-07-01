//! Image fetching. `http(s)://` URLs go over ureq + rustls (replacing both the bundled
//! `xh` binary and the wait-for-wifi ping loop — we retry the real request until Wi-Fi is
//! up); a `file://` URL is read straight off disk, which is handy for local testing.

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use std::time::{Duration, Instant};

/// PNG file signature — guards against a server returning a 200 with an HTML error page.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A ureq agent configured with rustls TLS and a per-request global timeout.
fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .into()
}

/// A fetch failure, classified by whether retrying within the same cycle can help.
enum FetchError {
    /// Connection/transport failure — worth retrying while Wi-Fi comes up.
    Transient(anyhow::Error),
    /// The server responded but the result is unusable (bad status, not a PNG, IO
    /// error) — retrying this cycle won't help; skip to the next scheduled wakeup.
    Permanent(anyhow::Error),
}

/// Fetch the dashboard image to `dest`, retrying transient failures until success or
/// `timeout_secs` elapses. Retrying the real request (rather than pinging) is how we wait
/// for Wi-Fi to re-associate after waking from suspend.
pub fn fetch_to(url: &str, dest: &Path, timeout_secs: u64) -> Result<()> {
    // A `file://` source is local: read it directly, no HTTP/retry machinery. Accepts the
    // `file:///abs/path` form (host component, if any, is not supported).
    if let Some(path) = url.strip_prefix("file://") {
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        return validate_and_write(&bytes, dest);
    }

    // Bound each request by the overall budget so a single hang can't exceed it (and so a
    // small budget really is small), capped so a generous budget still fails reasonably.
    let per_request = timeout_secs.clamp(1, 15);
    let agent = agent(per_request);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match try_fetch(&agent, url, dest) {
            Ok(()) => return Ok(()),
            Err(FetchError::Permanent(e)) => return Err(e),
            Err(FetchError::Transient(e)) if Instant::now() >= deadline => {
                return Err(e.context(format!("giving up after {attempt} attempt(s)")));
            }
            Err(FetchError::Transient(e)) => {
                log::warn!("fetch attempt {attempt} failed (retrying): {e:#}");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn try_fetch(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<(), FetchError> {
    let mut resp = agent
        .get(url)
        .call()
        .map_err(|e| FetchError::Transient(anyhow!("HTTP request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let code = status.as_u16();
        let err = anyhow!("server returned HTTP {code}");
        // A rate-limit or server/proxy hiccup can clear within our retry budget; other 4xx
        // (bad URL, auth) won't fix themselves this cycle.
        return Err(if status_is_transient(code) {
            FetchError::Transient(err)
        } else {
            FetchError::Permanent(err)
        });
    }

    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| FetchError::Transient(anyhow!("reading response body: {e}")))?;

    // A bad status was handled above; anything wrong from here on (not a PNG, IO error) is
    // permanent for this cycle.
    validate_and_write(&bytes, dest).map_err(FetchError::Permanent)
}

/// Whether a non-2xx HTTP status is worth retrying within the fetch budget: rate-limiting
/// (429) and server/proxy errors (5xx) are transient; other 4xx are permanent this cycle.
fn status_is_transient(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// Check the PNG magic bytes, then write to a temp file and rename into `dest`, so a
/// partial, non-PNG, or failed download never gets rendered to the screen.
fn validate_and_write(bytes: &[u8], dest: &Path) -> Result<()> {
    if !bytes.starts_with(PNG_MAGIC) {
        bail!("not a PNG image ({} bytes)", bytes.len());
    }
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming into {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid-looking PNG byte string (magic bytes are all `fetch_to` checks).
    fn png_bytes() -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(b"...image data...");
        v
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kindle-dash-fetch-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn http_status_classification() {
        // Retry rate-limits and server/proxy errors within the budget.
        assert!(status_is_transient(429));
        assert!(status_is_transient(500));
        assert!(status_is_transient(502));
        assert!(status_is_transient(503));
        assert!(status_is_transient(504));
        // Client errors (our fault) won't clear this cycle.
        assert!(!status_is_transient(400));
        assert!(!status_is_transient(401));
        assert!(!status_is_transient(404));
    }

    #[test]
    fn file_url_copies_a_valid_png() {
        let dir = scratch("copy-valid");
        let src = dir.join("src.png");
        let dest = dir.join("dash.png");
        let data = png_bytes();
        std::fs::write(&src, &data).unwrap();

        fetch_to(&format!("file://{}", src.display()), &dest, 5).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), data);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_url_rejects_non_png_and_leaves_dest_untouched() {
        let dir = scratch("reject-non-png");
        let src = dir.join("src.png");
        let dest = dir.join("dash.png");
        std::fs::write(&src, b"<html>not an image</html>").unwrap();

        assert!(fetch_to(&format!("file://{}", src.display()), &dest, 5).is_err());
        assert!(!dest.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_url_errors_on_missing_source() {
        let dir = scratch("missing-src");
        let dest = dir.join("dash.png");

        assert!(fetch_to(&format!("file://{}/nope.png", dir.display()), &dest, 5).is_err());
        assert!(!dest.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
