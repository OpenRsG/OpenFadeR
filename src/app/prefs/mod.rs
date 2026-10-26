//! User preferences ↔ ui/prefs.slint (global `PrefsState`, the PREFERENCES dialog).
//!
//! Stored as `key = value` lines in the settings file ([`file`]). Parsing: blank lines and `#` comments are
//! skipped; a missing key keeps its default silently; a known key with an invalid value falls back to its
//! default and yields a note (printed to stderr by [`load`]); **unknown keys are preserved** (rewritten after the
//! known ones, in file order). Comments are not kept.
//!
//! # Sections (one child per dialog category)
//! [`view`], [`audio`], [`analysis`], [`controller`], [`extensions`], [`keyboard`], [`advanced`] own typed
//! sections of [`Prefs`]; [`dj_system`] keeps its settings elsewhere ([`lowpower`] is the ADVANCED > Others LOW-POWER MODE setting's logic). Every category exports the same two
//! functions:
//!
//! * `pub fn rows(p: &Prefs) -> Vec<Row>`: the rows shown for the category (rebuilt after every click and
//!   every change, so a row may depend on other settings or on storage outside [`Prefs`]);
//! * `pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool`: a click on option `i` of the row `key`
//!   (true = the key was yours). Settings kept elsewhere (store.db) are written here too; the dialog redraws
//!   after every click whether or not [`Prefs`] changed.
//!
//! # Settings API for children
//! * Typed sections implement [`Section`] (`parse` / `write` of their `key = value` lines) and sit as a field
//!   of [`Prefs`] (see `Prefs::view`, `::audio`, `::analysis`).
//! * A child that does not want a typed field uses the generic string settings: [`Prefs::extra`],
//!   [`Prefs::set_extra`], [`Prefs::extra_bool`] (they live in the same file under the key you give; choose a
//!   unique `snake_case` key). Invalid values read as the default.
//! * Enums: the [`choice!`] macro (`Choice` = closed set with file names).
//! * Readers: [`get`] (UI thread, current values; clones), [`with`] (same, borrowed; for reading a field or two), [`on_change`] (after every real change), [`update`] (edit +
//!   save + notify), [`refresh`] (redraw the dialog rows after something outside [`Prefs`] changed).
//! * Rows: [`Row::chips`] (up to 4 short options), [`Row::dropdown`], [`Row::check`], [`Row::header`], [`Row::info`].
//!
//! The window size is remembered by a 0.5 s watcher in [`wire`].

pub mod advanced;
pub mod analysis;
pub mod audio;
pub mod controller;
mod dialog;
pub mod dj_system;
pub mod extensions;
mod file;
pub mod keyboard;
pub mod libraries;
mod live;
mod look;
pub mod lowpower;
mod panes;
mod rows;
mod section;
mod text;
pub mod view;

pub use dialog::{refresh, wire};
pub use file::{load, path, save};
#[cfg(test)]
pub use live::set_for_test;
pub use live::{get, notify, on_action, on_change, on_text, update, with};
pub use look::apply_theme;
pub use rows::Row;
pub use section::{Choice, Section, Setting};
pub(crate) use section::{choice, section};
pub use view::{FontSize, HotCueColors, KeyNotation, LineSpace, WaveColors};

use openfader::audio::fx::DEFAULT_BEAT;

choice!(
    /// Crossfader law. `Constant` is constant-power (what the mixer did before this setting existed),
    /// `Smooth` a linear gain fade, `Sharp` a cut (each side stays at full gain until the fader is
    /// within 10 % of the other end). The shapes are our choice.
    CrossfaderCurve { Smooth = "smooth", Sharp = "sharp", #[default] Constant = "constant" }
);

/// The choice named `v` (case-insensitive).
pub fn choice<T: Choice>(v: &str) -> Option<T> {
    T::ALL
        .iter()
        .copied()
        .find(|c| c.name().eq_ignore_ascii_case(v))
}

/// Names of the tempo ranges, indexing the deck's `transport::RANGES` (±6 %, ±10 %, ±16 %, WIDE).
const TEMPO_RANGES: [&str; 4] = ["6", "10", "16", "wide"];

/// Every setting: the typed category sections, the base fields below, and the keys no section knows.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub view: view::View,
    pub audio: audio::Audio,
    pub analysis: analysis::Analysis,
    pub controller: controller::Controller,
    pub extensions: extensions::Extensions,
    pub keyboard: keyboard::Keyboard,
    pub advanced: advanced::Advanced,
    pub key_notation: KeyNotation,
    pub waveform_colors: WaveColors,
    /// Index into [`TEMPO_RANGES`], applied to both decks at start-up.
    pub tempo_range_default: usize,
    pub quantize_default: bool,
    pub key_lock_default: bool,
    pub crossfader_curve: CrossfaderCurve,
    /// Logical pixels; 0 = not stored yet.
    pub window_w: u32,
    pub window_h: u32,
    /// Beat FX: selector index (`Effect::names()`: built-ins, then plugin effects; stored by name) and into `BEAT_TEXT`,
    /// restored at start-up; and FX1's tempo source (`false` = CH, the deck on its channel; `true` = MST, the master deck).
    pub fx_effect: usize,
    pub fx_beat: usize,
    pub fx_tempo_master: bool,
    /// Decks shown and playable: 2 (A, B) or 4 (A to D), see `deck::active`.
    pub decks: usize,
    /// STEMS (track separation, `openfader::stems`): off by default (switching it on downloads a 316 MB model once, `app/stems_model`, and it uses
    /// CPU); the stem set is 3 (VOCAL, INST, DRUMS: bass inside INST) or 4 stems; the ACTIVE PART buttons / pads mute
    /// or solo a part; the mute/solo state resets on LOAD.
    pub stems: bool,
    pub stems_four: bool,
    pub active_part_solo: bool,
    pub stems_reset: bool,
    /// STEMS Analysis Process ("Prioritize sound quality" / "Prioritize speed"): speed separates with less
    /// overlap between segments, see `openfader::stems::Quality`. Takes effect for the next separation.
    pub stems_fast: bool,
    /// STEM waveform: the deck waveforms show the stems heard (muted / soloed parts left out) instead of the original.
    pub stem_wave: bool,
    /// "Use controller audio when connected" (the connected DJ controller's audio interface becomes the output, the
    /// previous device comes back when it is unplugged). On by default; `OPENFADER_OUTPUT_DEVICE` still wins.
    pub controller_audio: bool,
    /// Keys no section knows (the generic [`Prefs::extra`] settings and keys of a newer version), kept so a save does not drop them.
    pub unknown: Vec<(String, String)>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            view: Default::default(),
            audio: Default::default(),
            analysis: Default::default(),
            controller: Default::default(),
            extensions: Default::default(),
            keyboard: Default::default(),
            advanced: Default::default(),
            key_notation: KeyNotation::default(),
            waveform_colors: WaveColors::default(),
            tempo_range_default: 0,
            quantize_default: true,
            key_lock_default: false,
            crossfader_curve: CrossfaderCurve::default(),
            window_w: 0,
            window_h: 0,
            fx_effect: 0,
            fx_beat: DEFAULT_BEAT,
            fx_tempo_master: false,
            decks: 2,
            stems: false,
            stems_fast: false,
            stem_wave: true,
            stems_four: true,
            active_part_solo: false,
            controller_audio: true,
            stems_reset: true,
            unknown: Vec::new(),
        }
    }
}

impl Prefs {
    /// The generic setting `key` (see the module doc), if present in the file or set.
    pub fn extra(&self, key: &str) -> Option<&str> {
        self.unknown
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Sets the generic setting `key` (replaces an earlier value).
    pub fn set_extra(&mut self, key: &str, v: impl ToString) {
        let v = v.to_string();
        match self.unknown.iter_mut().find(|(k, _)| k == key) {
            Some((_, old)) => *old = v,
            None => self.unknown.push((key.to_owned(), v)),
        }
    }

    pub fn extra_bool(&self, key: &str, default: bool) -> bool {
        self.extra(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// One dialog click on a setting that lives in this struct's base fields (option `i` of `field`; an
    /// out-of-range index changes nothing); false = not one of these keys.
    pub fn choose(&mut self, field: &str, i: usize) -> bool {
        fn pick<T: Choice>(slot: &mut T, i: usize) {
            if let Some(&v) = T::ALL.get(i) {
                *slot = v;
            }
        }
        match field {
            "key" => pick(&mut self.key_notation, i),
            "wave" => pick(&mut self.waveform_colors, i),
            "curve" => pick(&mut self.crossfader_curve, i),
            "range" if i < TEMPO_RANGES.len() => self.tempo_range_default = i,
            "quantize" => self.quantize_default = i == 0, // option 0 = ON
            "keylock" => self.key_lock_default = i == 0,
            "decks" => self.decks = [2, 4][i.min(1)], // option 0 = 2 DECKS, 1 = 4 DECKS
            "stems" => self.stems = i == 1,           // option 0 = OFF, 1 = ON
            "stems_mode" => self.stems_four = i == 1, // option 0 = 3 STEMS, 1 = 4 STEMS
            "active_part" => self.active_part_solo = i == 1, // option 0 = MUTE, 1 = SOLO
            "stems_reset" => self.stems_reset = i == 0, // option 0 = RESET, 1 = KEEP
            "stems_analysis" => self.stems_fast = i == 1, // option 0 = QUALITY, 1 = SPEED
            "stem_waveform" => self.stem_wave = i == 1, // option 0 = ORIGINAL, 1 = STEM
            "controller_audio" => self.controller_audio = i == 0, // option 0 = ON, 1 = OFF
            "range" => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests;
