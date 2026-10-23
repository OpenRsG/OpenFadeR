//! The settings file text: [`Prefs::parse`] and [`Prefs::to_text`] for the fields [`Prefs`] holds itself; the typed
//! sections write and read their own lines.

use super::{Choice, Prefs, Section, TEMPO_RANGES, choice};
use openfader::audio::fx::{BEAT_TEXT, Effect};

/// File names of the two-valued settings, `[false, true]`.
const STEMS_MODE: [&str; 2] = ["3", "4"];
const ACTIVE_PART: [&str; 2] = ["mute", "solo"];
const STEMS_ANALYSIS: [&str; 2] = ["quality", "speed"];
const STEM_WAVEFORM: [&str; 2] = ["original", "stem"];

/// `v` as a choice index into `names` (case-insensitive).
pub(super) fn index_of(names: &[&str], v: &str) -> Option<usize> {
    names.iter().position(|n| n.eq_ignore_ascii_case(v))
}

/// Which of the two `names` `v` is: false = the first.
fn which(names: &[&str; 2], v: &str) -> Option<bool> {
    index_of(names, v).map(|i| i == 1)
}

fn pixels(v: &str) -> Option<u32> {
    v.parse().ok().filter(|&n| n <= 16384)
}

/// Stored by name, so adding or removing a plugin does not move the saved choice (the built-ins keep their old names).
fn effect_name(i: usize) -> String {
    Effect::names()
        .get(i)
        .map_or_else(|| Effect::NAMES[0].to_lowercase(), |n| n.to_lowercase())
}

impl Prefs {
    pub(super) fn sections(&mut self) -> [&mut dyn Section; 7] {
        [
            &mut self.view,
            &mut self.audio,
            &mut self.analysis,
            &mut self.controller,
            &mut self.extensions,
            &mut self.keyboard,
            &mut self.advanced,
        ]
    }

    /// Parses a settings file; the notes describe every line that was ignored or reset.
    pub fn parse(text: &str) -> (Prefs, Vec<String>) {
        let (mut p, d, mut notes) = (Prefs::default(), Prefs::default(), Vec::new());
        for (n, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                notes.push(format!("line {n}: not `key = value`, ignored"));
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            // An invalid value resets the field to its default and is noted.
            macro_rules! set {
                ($field:ident, $parsed:expr) => {
                    p.$field = $parsed.unwrap_or_else(|| {
                        notes.push(format!("{k}: invalid value {v:?}, using the default"));
                        d.$field
                    })
                };
            }
            match k {
                "key_notation" => set!(key_notation, choice(v)),
                "waveform_colors" => set!(waveform_colors, choice(v)),
                "crossfader_curve" => set!(crossfader_curve, choice(v)),
                "tempo_range_default" => set!(tempo_range_default, index_of(&TEMPO_RANGES, v)),
                "quantize_default" => set!(quantize_default, v.parse().ok()),
                "key_lock_default" => set!(key_lock_default, v.parse().ok()),
                "window_w" => set!(window_w, pixels(v)),
                "window_h" => set!(window_h, pixels(v)),
                "fx_effect" => set!(
                    fx_effect,
                    Effect::names()
                        .iter()
                        .position(|n| n.to_lowercase() == v.to_lowercase())
                ),
                "fx_beat" => set!(fx_beat, BEAT_TEXT.iter().position(|&b| b == v)),
                "fx_tempo_master" => set!(fx_tempo_master, v.parse().ok()),
                "decks" => set!(decks, v.parse().ok().filter(|n| [2, 4].contains(n))),
                "stems" => set!(stems, v.parse().ok()),
                "stems_mode" => set!(stems_four, which(&STEMS_MODE, v)),
                "active_part" => set!(active_part_solo, which(&ACTIVE_PART, v)),
                "stems_reset_on_load" => set!(stems_reset, v.parse().ok()),
                "stems_analysis" => set!(stems_fast, which(&STEMS_ANALYSIS, v)),
                "stem_waveform" => set!(stem_wave, which(&STEM_WAVEFORM, v)),
                "controller_audio" => set!(controller_audio, v.parse().ok()),
                _ => {
                    let handled = p.sections().into_iter().find_map(|s| s.parse(k, v));
                    match handled {
                        Some(true) => {}
                        Some(false) => {
                            notes.push(format!("{k}: invalid value {v:?}, using the default"))
                        }
                        None => p.unknown.push((k.to_owned(), v.to_owned())),
                    }
                }
            }
        }
        (p, notes)
    }

    /// The settings file text; `parse(to_text())` gives the same `Prefs`.
    pub fn to_text(&self) -> String {
        let mut s = String::from(
            "# OpenFadeR settings, one `key = value` per line (rewritten on every change)\n",
        );
        let mut line = |k: &str, v: &str| {
            s.push_str(k);
            s.push_str(" = ");
            s.push_str(v);
            s.push('\n');
        };
        line("key_notation", self.key_notation.name());
        line("waveform_colors", self.waveform_colors.name());
        line(
            "tempo_range_default",
            TEMPO_RANGES[self.tempo_range_default],
        );
        line("quantize_default", &self.quantize_default.to_string());
        line("key_lock_default", &self.key_lock_default.to_string());
        line("crossfader_curve", self.crossfader_curve.name());
        line("window_w", &self.window_w.to_string());
        line("window_h", &self.window_h.to_string());
        line("fx_effect", &effect_name(self.fx_effect));
        line("fx_beat", BEAT_TEXT[self.fx_beat]);
        line("fx_tempo_master", &self.fx_tempo_master.to_string());
        line("decks", &self.decks.to_string());
        line("stems", &self.stems.to_string());
        line("stems_mode", STEMS_MODE[self.stems_four as usize]);
        line("active_part", ACTIVE_PART[self.active_part_solo as usize]);
        line("stems_reset_on_load", &self.stems_reset.to_string());
        line("stems_analysis", STEMS_ANALYSIS[self.stems_fast as usize]);
        line("stem_waveform", STEM_WAVEFORM[self.stem_wave as usize]);
        line("controller_audio", &self.controller_audio.to_string());
        let sections: [&dyn Section; 7] = [
            &self.view,
            &self.audio,
            &self.analysis,
            &self.controller,
            &self.extensions,
            &self.keyboard,
            &self.advanced,
        ];
        for sec in sections {
            sec.write(&mut line);
        }
        for (k, v) in &self.unknown {
            line(k, v);
        }
        s
    }
}
