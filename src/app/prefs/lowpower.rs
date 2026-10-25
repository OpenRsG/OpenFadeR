//! The low-power device profile (Preferences > Advanced > Others > LOW-POWER MODE: AUTO / ON / OFF) for a Raspberry Pi or another small machine
//! (issue #24). AUTO follows [`detect`]; ON and OFF override it. [`active`] is the effective answer, read where the profile changes behaviour:
//!
//! | where | low-power mode |
//! | --- | --- |
//! | automatic audio buffer ([`super::audio`]) | driver default instead of the low-latency fixed buffer; explicit frame counts still win |
//! | sampler ([`crate::app::sampler`]) | a slot keeps at most [`SAMPLER_SECS`] instead of 30 s (16 slots at 48 kHz stereo f32: 180 MB down to 60 MB) |
//! | analysis workers ([`crate::app::analyze::workers`]) | at most [`ANALYSIS_WORKERS`] ([`analysis_workers`]); each worker holds a whole decoded track |
//!
//! Decks, previews and analysis already decode to 16-bit [`openfader::audio::Track16`] on every machine, so the profile does not switch that.

use super::{Prefs, choice};
use std::sync::LazyLock;

choice!(
    /// Low-power mode: `Auto` = detect the machine ([`detect`]), `On` / `Off` = forced.
    LowPower { #[default] Auto = "auto", On = "on", Off = "off" }
);

/// Longest slice a sampler slot keeps in low-power mode, seconds (30 s otherwise: `openfader::audio::sampler::MAX_SECS`).
pub const SAMPLER_SECS: f64 = 10.0;
/// Analysis workers at most in low-power mode (our choice: the Pi 4's four cores are shared with the UI and the audio thread).
pub const ANALYSIS_WORKERS: usize = 2;
/// A machine with this much RAM or less counts as small (8 GiB, in KiB as `/proc/meminfo` gives it; a Pi's 8 GB board reports a bit less).
const SMALL_RAM_KIB: u64 = 8 * 1024 * 1024;

/// Is this a small machine: a Raspberry Pi (its device-tree `model` says so), or Linux on 64-bit / 32-bit ARM
/// (`std::env::consts::ARCH`: `aarch64`, `arm`) with at most 8 GB of RAM. Unknown RAM does not count as small.
pub fn detect(os: &str, arch: &str, ram_kib: Option<u64>, model: Option<&str>) -> bool {
    os == "linux"
        && (model.is_some_and(|m| m.contains("Raspberry Pi"))
            || (matches!(arch, "aarch64" | "arm") && ram_kib.is_some_and(|k| k <= SMALL_RAM_KIB)))
}

/// `MemTotal` of a `/proc/meminfo` text, KiB.
fn mem_total_kib(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// [`detect`] for this machine (read once: neither the hardware nor the architecture changes while we run).
static DETECTED: LazyLock<bool> = LazyLock::new(|| {
    let ram = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| mem_total_kib(&t));
    let model = std::fs::read_to_string("/proc/device-tree/model").ok();
    detect(
        std::env::consts::OS,
        std::env::consts::ARCH,
        ram,
        model.as_deref(),
    )
});

/// The effective mode of `setting` when detection says `detected`.
pub fn resolve(setting: LowPower, detected: bool) -> bool {
    match setting {
        LowPower::Auto => detected,
        LowPower::On => true,
        LowPower::Off => false,
    }
}

/// Is low-power mode on for these settings (the setting, else detection).
pub fn on(p: &Prefs) -> bool {
    resolve(p.advanced.low_power, *DETECTED)
}

/// [`on`] for the current settings (UI thread, like [`super::with`]).
pub fn active() -> bool {
    super::with(on)
}

/// `n` analysis workers, fewer in low-power mode.
pub fn analysis_workers(n: usize) -> usize {
    if active() { n.min(ANALYSIS_WORKERS) } else { n }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::Section;

    const GIB: u64 = 1024 * 1024;

    #[test]
    fn a_raspberry_pi_or_a_small_arm_linux_box_is_low_power() {
        let pi = Some("Raspberry Pi 4 Model B Rev 1.5\0");
        assert!(detect("linux", "aarch64", Some(4 * GIB), pi));
        assert!(
            detect("linux", "aarch64", None, pi),
            "the model alone is enough"
        );
        assert!(
            detect("linux", "aarch64", Some(7_900_000), None),
            "an 8 GB board reports a little less than 8 GiB"
        );
        assert!(detect("linux", "arm", Some(GIB), None), "armv7");
        assert!(detect(
            "linux",
            "aarch64",
            Some(8 * GIB),
            Some("Pine64 RockPro64")
        ));
    }

    #[test]
    fn big_x86_and_non_linux_machines_are_not() {
        assert!(
            !detect("linux", "x86_64", Some(4 * GIB), None),
            "small x86 is a PC, not a Pi"
        );
        assert!(!detect(
            "linux",
            "aarch64",
            Some(16 * GIB),
            Some("Apple MacBook Air")
        ));
        assert!(!detect("linux", "aarch64", None, None), "unknown RAM");
        assert!(!detect("macos", "aarch64", Some(8 * GIB), None));
        assert!(!detect(
            "windows",
            "aarch64",
            Some(4 * GIB),
            Some("Raspberry Pi")
        ));
    }

    #[test]
    fn meminfo_gives_the_total() {
        let t = "MemTotal:        7918300 kB\nMemFree:          123 kB\n";
        assert_eq!(mem_total_kib(t), Some(7_918_300));
        assert_eq!(mem_total_kib("MemFree: 1 kB\n"), None);
        assert_eq!(mem_total_kib("MemTotal: lots kB\n"), None);
    }

    #[test]
    fn auto_follows_detection_and_on_off_override_it() {
        for detected in [false, true] {
            assert_eq!(resolve(LowPower::Auto, detected), detected);
            assert!(resolve(LowPower::On, detected));
            assert!(!resolve(LowPower::Off, detected));
        }
    }

    #[test]
    fn the_setting_survives_the_settings_file() {
        let mut a = crate::app::prefs::advanced::Advanced::default();
        assert_eq!(a.low_power, LowPower::Auto);
        assert_eq!(a.parse("low_power", "on"), Some(true));
        assert_eq!(a.low_power, LowPower::On);
        let mut lines = Vec::new();
        a.write(&mut |k, v| lines.push(format!("{k}={v}")));
        assert!(lines.contains(&"low_power=on".to_owned()));
        assert_eq!(a.parse("low_power", "sometimes"), Some(false));
        assert_eq!(
            a.low_power,
            LowPower::Auto,
            "an unknown value is the default"
        );
        assert!(
            a.pick("low_power", 2) && a.low_power == LowPower::Off,
            "option 2 = OFF"
        );
    }
}
