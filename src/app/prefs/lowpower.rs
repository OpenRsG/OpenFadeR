//! The low-power device profile (Preferences > Advanced > Others > LOW-POWER MODE: AUTO / ON / OFF).
//! AUTO covers Raspberry Pi and machines with at most 8 GiB of effective RAM; ON and OFF override it.
//!
//! | where | low-power mode |
//! | --- | --- |
//! | automatic audio buffer ([`super::audio`]) | driver default instead of the low-latency fixed buffer; explicit frame counts still win |
//! | sampler ([`crate::app::sampler`]) | a slot keeps at most [`SAMPLER_SECS`] instead of 30 s (16 slots at 48 kHz stereo f32: 180 MB down to 60 MB) |
//! | analysis workers ([`crate::app::analyze::workers`]) | at most [`ANALYSIS_WORKERS`] ([`analysis_workers`]); each worker holds a whole decoded track |
//! | stem inference | one batch, at most two CPU threads, no concurrent CPU/GPU scratch |
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
/// A machine at or below the shared memory budget counts as small (including cgroup-limited machines).
const SMALL_RAM_KIB: u64 = openfader::memory::SMALL_BYTES / 1024;

/// Raspberry Pi remains low-power even on larger boards; RAM policy applies to every OS and architecture.
pub fn detect(os: &str, ram_kib: Option<u64>, model: Option<&str>) -> bool {
    (os == "linux" && model.is_some_and(|m| m.contains("Raspberry Pi")))
        || ram_kib.is_some_and(|k| k <= SMALL_RAM_KIB)
}

static DETECTED: LazyLock<bool> = LazyLock::new(|| {
    let ram = openfader::memory::total_bytes().map(|bytes| bytes / 1024);
    let model = std::fs::read_to_string("/proc/device-tree/model").ok();
    detect(std::env::consts::OS, ram, model.as_deref())
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

#[cfg(feature = "stems")]
pub(super) fn wire() {
    let apply = |p: &Prefs| {
        let limited = match p.advanced.low_power {
            LowPower::Auto => on(p) || openfader::memory::constrained(),
            LowPower::On => true,
            LowPower::Off => false,
        };
        openfader::stems::model::set_low_memory(limited);
    };
    super::with(apply);
    super::on_change(apply);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::Section;

    const GIB: u64 = 1024 * 1024;

    #[test]
    fn raspberry_pi_and_small_memory_devices_share_the_profile() {
        let pi = Some("Raspberry Pi 5 Model B Rev 1.0\0");
        assert!(detect("linux", Some(16 * GIB), pi));
        assert!(detect("linux", None, pi));
        for os in ["linux", "macos", "windows"] {
            assert!(detect(os, Some(4 * GIB), None));
            assert!(detect(os, Some(8 * GIB), None));
            assert!(!detect(os, Some(8 * GIB + 1), None));
            assert!(!detect(os, None, None));
        }
        assert!(!detect("windows", Some(16 * GIB), pi));
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
