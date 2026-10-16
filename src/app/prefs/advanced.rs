//! ADVANCED category (Preferences > Advanced: Database, Browse, Recordings, Others). The typed section [`Advanced`] holds the RECORDINGS tab (the same settings as CONTROLLER > Recordings) and the sampling frequency of Export Loop As Wav. DATABASE and
//! BROWSE are the library's ([`crate::app::library::settings`]); BEAT/BPM SYNC and HOT CUE are the CONTROLLER >
//! Deck settings shown again. Streaming services, Tribe XR and PRO DJ LINK items need a network and are left out.

use super::{section, Prefs, Row};
use crate::app::library::settings as library;
use openfader::audio::RecOptions;
use std::path::PathBuf;

/// Silence Level Threshold choices, dBFS.
const THRESHOLDS: [i32; 11] = [-80, -75, -70, -65, -60, -55, -50, -45, -40, -35, -30];
const DEFAULT_THRESHOLD: i32 = -60;
const DEFAULT_LOOP_HZ: u32 = 44_100;
/// Export Loop As Wav sampling frequencies, Hz.
const LOOP_RATES: [u32; 4] = [DEFAULT_LOOP_HZ, 48_000, 88_200, 96_000];

section!(
    Advanced {
        /// Recordings > Location of Files; empty = `$OPENFADER_REC_DIR`, else `~/Music/OpenFadeR`.
        rec_location: String = String::new();
        /// Silence Level Threshold in dBFS, one of [`THRESHOLDS`].
        rec_threshold: i32 = DEFAULT_THRESHOLD;
        /// Recording starts when: audio is detected / ends when: 20 s of silence.
        rec_start_on_audio: bool = true;
        rec_end_on_silence: bool = true;
        /// Automatically separate files when silence continues, for `rec_split_secs` (1 to 10 s).
        rec_split: bool = true;
        rec_split_secs: u8 = 2;
        /// Automatically optimize the audio level of a recorded file.
        rec_optimise: bool = true;
        /// Automatically import a recorded file / display its tag information.
        rec_import: bool = true;
        rec_tag_window: bool = true;
        /// Export Loop As Wav: sampling frequency in Hz, one of [`LOOP_RATES`].
        export_loop_hz: u32 = DEFAULT_LOOP_HZ;
    }
);

impl Advanced {
    /// The recorder's gate, from the RECORDINGS tab.
    pub fn rec_options(&self) -> RecOptions {
        RecOptions {
            start_on_audio: self.rec_start_on_audio,
            end_on_silence: self.rec_end_on_silence,
            threshold_db: self.threshold() as f32,
            split_after_s: self
                .rec_split
                .then(|| self.rec_split_secs.clamp(1, 10) as u32),
            optimise: self.rec_optimise,
        }
    }

    /// `rec_threshold` if it is one of [`THRESHOLDS`] (a hand-edited file may hold anything), else the default.
    fn threshold(&self) -> i32 {
        if THRESHOLDS.contains(&self.rec_threshold) {
            self.rec_threshold
        } else {
            DEFAULT_THRESHOLD
        }
    }

    /// Location of Files, `None` = the default folder.
    pub fn rec_folder(&self) -> Option<PathBuf> {
        Some(self.rec_location.trim())
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    }

    /// Sampling frequency of a loop exported as WAV.
    pub fn loop_hz(&self) -> u32 {
        if LOOP_RATES.contains(&self.export_loop_hz) {
            self.export_loop_hz
        } else {
            DEFAULT_LOOP_HZ
        }
    }

    fn pick_number(&mut self, key: &str, i: usize) -> bool {
        match key {
            "rec_threshold" => THRESHOLDS.get(i).map(|&v| self.rec_threshold = v).is_some(),
            "rec_split_secs" => (i < 10)
                .then(|| self.rec_split_secs = i as u8 + 1)
                .is_some(),
            "export_loop_hz" => LOOP_RATES
                .get(i)
                .map(|&v| self.export_loop_hz = v)
                .is_some(),
            _ => false,
        }
    }
}

/// The RECORDINGS tab rows (shown in CONTROLLER and in ADVANCED).
pub fn recording_rows(a: &Advanced) -> Vec<Row> {
    let db: Vec<String> = THRESHOLDS.iter().map(|d| format!("{d} dB")).collect();
    let secs: Vec<String> = (1..=10).map(|s| format!("{s} s")).collect();
    vec![
        Row::text("rec_location", "LOCATION OF FILES", &a.rec_location),
        Row::info("", "empty = ~/Music/OpenFadeR"),
        Row::dropdown_of(
            "rec_threshold",
            "SILENCE LEVEL THRESHOLD",
            db,
            THRESHOLDS
                .iter()
                .position(|&d| d == a.threshold())
                .unwrap_or(0),
        ),
        Row::chips(
            "rec_start_on_audio",
            "RECORDING STARTS WHEN",
            &["REC BUTTON", "AUDIO INPUT"],
            a.rec_start_on_audio as usize,
        ),
        Row::chips(
            "rec_end_on_silence",
            "RECORDING ENDS WHEN",
            &["REC BUTTON", "20 S SILENCE"],
            a.rec_end_on_silence as usize,
        ),
        Row::check(
            "rec_split",
            "Automatically separate files when silence continues",
            a.rec_split,
        ),
        Row::dropdown_of(
            "rec_split_secs",
            "SILENCE LENGTH",
            secs,
            a.rec_split_secs.clamp(1, 10) as usize - 1,
        )
        .enabled(a.rec_split),
        Row::check(
            "rec_optimise",
            "Automatically optimize the audio level of a recorded file",
            a.rec_optimise,
        ),
        Row::check(
            "rec_import",
            "Automatically import a recorded file",
            a.rec_import,
        ),
        Row::check(
            "rec_tag_window",
            "Automatically display the tag information of a recorded file",
            a.rec_tag_window,
        ),
    ]
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let (c, a) = (&p.controller, &p.advanced);
    let at = LOOP_RATES
        .iter()
        .position(|&h| h == a.loop_hz())
        .unwrap_or(0);
    let rates: Vec<String> = LOOP_RATES.iter().map(|h| format!("{h} Hz")).collect();
    // The database and browse rows bring their own headings.
    let mut rows = library::database_rows(p);
    rows.extend(library::browse_rows(p));
    rows.push(Row::header("RECORDINGS"));
    rows.extend(recording_rows(a));
    rows.extend([
        Row::header("OTHERS"),
        Row::of("sync_type", "SYNC TYPE", c.sync_type),
        Row::check(
            "sync_half_double",
            "Allow BEAT/BPM SYNC with double/half BPM",
            c.sync_half_double,
        ),
        Row::check(
            "hot_cue_gate",
            "HOT CUE: during pause, GATE playback is applied",
            c.hot_cue_gate,
        ),
        Row::dropdown_of("export_loop_hz", "EXPORT LOOP AS WAV", rates, at)
            .hint("sampling frequency"),
    ]);
    rows
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    p.advanced.pick_number(key, i)
        || p.advanced.pick(key, i)
        || ["sync_type", "sync_half_double", "hot_cue_gate"].contains(&key)
            && p.controller.pick(key, i)
        || library::database_choose(p, key, i)
        || library::browse_choose(p, key, i)
}

/// Registers the text rows of this page (the recordings folder).
pub fn wire() {
    super::on_text("rec_location", |v| {
        super::update(|p| p.advanced.rec_location = v.trim().to_owned())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_follow_the_tab() {
        let a = Advanced {
            rec_start_on_audio: false,
            rec_split: false,
            rec_threshold: -45,
            rec_optimise: false,
            ..Default::default()
        };
        let o = a.rec_options();
        assert_eq!(
            (
                o.start_on_audio,
                o.split_after_s,
                o.threshold_db,
                o.optimise
            ),
            (false, None, -45.0, false)
        );
        let bad = Advanced {
            rec_threshold: -1,
            rec_split_secs: 99,
            export_loop_hz: 7,
            ..Default::default()
        };
        assert_eq!(
            (
                bad.rec_options().threshold_db,
                bad.rec_options().split_after_s,
                bad.loop_hz()
            ),
            (-60.0, Some(10), 44_100)
        );
    }

    #[test]
    fn folder_is_none_until_set() {
        assert_eq!(Advanced::default().rec_folder(), None);
        let a = Advanced {
            rec_location: "  /tmp/rec ".into(),
            ..Default::default()
        };
        assert_eq!(a.rec_folder(), Some(PathBuf::from("/tmp/rec")));
    }

    #[test]
    fn dropdown_clicks_set_numbers_and_booleans_through_choose() {
        let mut p = Prefs::default();
        assert!(choose(&mut p, "rec_threshold", 0) && p.advanced.rec_threshold == -80);
        assert!(choose(&mut p, "rec_split_secs", 9) && p.advanced.rec_split_secs == 10);
        assert!(choose(&mut p, "export_loop_hz", 3) && p.advanced.loop_hz() == 96_000);
        assert!(choose(&mut p, "rec_optimise", 0) && !p.advanced.rec_optimise);
        assert!(
            choose(&mut p, "sync_type", 1)
                && p.controller.sync_type == crate::app::prefs::controller::SyncType::Bpm,
            "sync_type is the Deck setting"
        );
        assert!(!choose(&mut p, "nope", 0));
    }
}
