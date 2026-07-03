//! Device detection and the model-agnostic device API. Only the Kindle Voyage is
//! supported; the binary hard-errors on anything else so it never pokes unverified sysfs
//! paths. All model-specific behavior (rendering, battery, power, suspend) lives behind
//! methods on `Device` — split across the submodules here — so business logic never names
//! a device command, sysfs path, or resolution directly.

mod battery;
mod display;
mod power;
mod sys;

pub use display::RefreshCycle;
pub use power::WakeReason;

use anyhow::{Context, Result, bail};

/// A supported Kindle model. Each variant carries the device-specific values and behavior
/// the rest of the program needs, exposed through the methods on this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Voyage,
}

impl Device {
    /// Display resolution as a `WIDTHxHEIGHT` string. Used as the assets subdirectory
    /// name, so bundled images live under `assets/<resolution>/`.
    pub fn resolution(self) -> &'static str {
        match self {
            Device::Voyage => "1448x1072",
        }
    }

    /// Standard Linux RTC sysfs alarm interfaces (absolute epoch seconds). On the Voyage
    /// both `rtc0` and `rtc1` are the same max77696 PMIC RTC (rtc2 is the SoC snvs); we
    /// arm both as a hedge since the exact suspend-wake node can't be verified without
    /// actually suspending. (`rtc0` is `hctosys`, the system clock.)
    fn rtc_wakealarm_paths(self) -> &'static [&'static str] {
        match self {
            Device::Voyage => &[
                "/sys/class/rtc/rtc0/wakealarm",
                "/sys/class/rtc/rtc1/wakealarm",
            ],
        }
    }

    /// Upstart job that owns the UI/framebuffer; stopped while the dashboard runs so it
    /// doesn't fight for the screen or drain the battery. On the Voyage this is
    /// `framework` (stopping `appmgrd` alone would not free the framebuffer).
    fn framework_service(self) -> &'static str {
        match self {
            Device::Voyage => "framework",
        }
    }
}

/// Voyage device codes from the MobileRead Kindle Serial Numbers wiki. Modern serials
/// embed a 4-char device code; each model has a `B0xx` code and, on newer production, a
/// `90xx` code.
const VOYAGE_CODES: &[&str] = &[
    "B013", "9013", // WiFi
    "B054", "9054", // 3G + WiFi (U.S.)
    "B053", "9053", // 3G + WiFi (Europe)
    "B02A", // 3G + WiFi (Japan)
    "B052", "9052", // 3G + WiFi (Mexico)
    "B04F", // 3G
];

/// Detect the running device, erroring out if it isn't on the supported allowlist.
///
/// Honors a `KINDLE_DEVICE` override (currently only `voyage`) for cases where serial
/// detection misfires or when running off-device for testing.
pub fn detect() -> Result<Device> {
    if let Ok(override_val) = std::env::var("KINDLE_DEVICE") {
        return parse_override(&override_val);
    }
    let serial = read_serial().context("could not read device serial number")?;
    match_device(&serial).with_context(|| format!("unsupported device (serial {serial:?})"))
}

fn parse_override(val: &str) -> Result<Device> {
    match val.to_lowercase().as_str() {
        "voyage" => Ok(Device::Voyage),
        other => bail!("KINDLE_DEVICE={other:?} is not a supported device"),
    }
}

/// Read the device serial (DSN): first from the running system service via the `usid`
/// property, then from the `Serial` line in `/proc/cpuinfo` if LIPC isn't up.
///
/// NOTE: `com.lab126.system serialNumber` and `/var/local/.../device.info` do NOT exist
/// on the Voyage — `usid` is the property that works (verified on-device).
fn read_serial() -> Result<String> {
    if let Ok(s) = sys::run("lipc-get-prop", &["com.lab126.system", "usid"]) {
        let s = clean_serial(&s);
        if !s.is_empty() {
            return Ok(s);
        }
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").context("reading /proc/cpuinfo")?;
    parse_cpuinfo_serial(&cpuinfo).context("no Serial line found in /proc/cpuinfo")
}

/// Strip surrounding whitespace and quotes from a serial value. `/proc/cpuinfo` reports
/// it quoted (e.g. `Serial : "90130907612700GM"`).
fn clean_serial(raw: &str) -> String {
    raw.trim().trim_matches('"').trim().to_string()
}

/// Match a serial against the supported allowlist.
fn match_device(serial: &str) -> Option<Device> {
    let upper = serial.to_uppercase();
    // The device code is embedded in the serial; substring match tolerates uncertainty
    // about its exact offset (validate the position on real hardware).
    if VOYAGE_CODES.iter().any(|code| upper.contains(code)) {
        Some(Device::Voyage)
    } else {
        None
    }
}

/// Extract the value of the `Serial` field from `/proc/cpuinfo` contents. The value is
/// reported quoted, so surrounding quotes are stripped.
fn parse_cpuinfo_serial(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "Serial").then(|| clean_serial(value))
        })
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_voyage_by_embedded_code() {
        assert_eq!(match_device("G090B013XXXXXXXX"), Some(Device::Voyage));
        assert_eq!(match_device("B053ABCDEF012345"), Some(Device::Voyage));
        assert_eq!(match_device("90540000ABCDEF01"), Some(Device::Voyage));
    }

    #[test]
    fn rejects_non_voyage_serial() {
        // A serial with no Voyage code (e.g. a Paperwhite-ish placeholder).
        assert_eq!(match_device("G000GGGG11112222"), None);
    }

    #[test]
    fn override_parsing() {
        assert_eq!(parse_override("voyage").unwrap(), Device::Voyage);
        assert_eq!(parse_override("Voyage").unwrap(), Device::Voyage);
        assert!(parse_override("paperwhite").is_err());
    }

    #[test]
    fn parses_quoted_serial_from_real_cpuinfo() {
        // Exact line seen on a Voyage: value is quoted.
        let cpuinfo = "processor\t: 0\nHardware\t: Kindle\nSerial\t\t: \"90130907612700GM\"\n";
        let serial = parse_cpuinfo_serial(cpuinfo).unwrap();
        assert_eq!(serial, "90130907612700GM"); // quotes stripped
        assert_eq!(match_device(&serial), Some(Device::Voyage)); // and it detects as Voyage
    }

    #[test]
    fn cpuinfo_without_serial_is_none() {
        assert_eq!(
            parse_cpuinfo_serial("processor\t: 0\nHardware\t: Kindle\n"),
            None
        );
        // An empty Serial value is treated as absent.
        assert_eq!(parse_cpuinfo_serial("Serial\t\t: \n"), None);
    }
}
