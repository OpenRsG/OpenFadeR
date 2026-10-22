//! CONTROLLER category (Preferences > Controller: Deck, Mixer, Effect, Sampler, Recordings, Others).
//! One typed section [`Controller`]
//! (every field is stored under its own name); the page is the tabs one below the other, headed by a
//! [`Row::header`]. Consumers read `prefs::with(|p| p.controller.*)` and the helpers below, which turn a stored choice into
//! the number the behaviour needs (values and defaults are ours).
//!
//! Children: [`deck`], [`mixer`], [`effect`], [`sampler`], [`others`] (rows of one tab each, numeric picks);
//! the RECORDINGS tab is the same settings as ADVANCED > Recordings ([`super::advanced`]).
//! MIX POINT LINK: the settings of the panel in [`crate::app::deck::mixpoint`], tab rows in [`mixpoint`].

mod deck;
mod effect;
mod mixer;
mod mixpoint;
mod named;
mod others;
mod sampler;

use super::{Prefs, Row, choice, section};
pub use named::{Fader, KnobMode};

choice!(
    /// Sync Type: BEAT SYNC follows tempo and beat phase, BPM SYNC only the tempo.
    SyncType { #[default] Beat = "beat", Bpm = "bpm" }
);
choice!(
    /// JOG rotation speed: how far the platter turns the track per revolution (×0.5 / ×1 / ×2).
    JogSpeed { Slow = "slow", #[default] Normal = "normal", Fast = "fast" }
);
choice!(
    /// Mixer > EQ: EQ (bands go down to -26 dB, never to silence) or ISOLATOR (a band at the bottom is a kill).
    EqType { #[default] Eq = "eq", Isolator = "isolator" }
);
choice!(
    /// Channel Fader Curve.
    FaderCurve { Steep = "steep", #[default] Linear = "linear", Gradual = "gradual" }
);
choice!(
    /// Quantize Mode: call a hot cue / loop at once with the beat phase kept, or after playing on to the next beat.
    QuantizeMode { #[default] Immediate = "immediate", NextBeat = "next-beat" }
);
choice!(
    /// Beat length of SNAP and QUANTIZE.
    BeatLength { Eighth = "1/8", Quarter = "1/4", Half = "1/2", #[default] One = "1" }
);
choice!(
    /// MIX IN / MIX OUT selection a MIX POINT LINK session starts in.
    MixPointDefault { #[default] In = "mix-in", Out = "mix-out" }
);

impl BeatLength {
    /// The length in beats.
    pub fn beats(self) -> f64 {
        match self {
            BeatLength::Eighth => 0.125,
            BeatLength::Quarter => 0.25,
            BeatLength::Half => 0.5,
            BeatLength::One => 1.0,
        }
    }
}

/// Auto Cue Level choices, dB.
pub const AUTO_CUE_DB: [i32; 8] = [-78, -72, -66, -60, -54, -48, -42, -36];
/// Vinyl Speed Adjust steps 1..=5 (slow … fast): seconds the platter takes to stop / to get back to speed (ours; step 3 is the 1 s BRAKE used before this setting existed).
pub const VINYL_SECONDS: [f32; 5] = [4.0, 2.0, 1.0, 0.5, 0.25];
/// Auto Beat Loop lengths, beats.
pub const LOOP_BEATS: [u32; 6] = [1, 2, 4, 8, 16, 32];
/// Output Level, dB, Internal mixer mode; 0 is NONE.
pub const OUTPUT_DB: [i32; 8] = [-21, -18, -15, -12, -9, -6, -3, 0];
/// Sampler slot counts.
pub const SAMPLER_SLOTS: [u8; 3] = [4, 8, 16];
/// SLICER range (LENGTH) in bars and ROLL as 1/n of it.
pub const SLICER_BARS: [u8; 4] = [1, 2, 4, 8];
pub const SLICER_ROLL: [u8; 4] = [8, 16, 32, 64];

section!(
    /// The CONTROLLER settings.
    Controller {
        /// Deck > Eject/Load Lock, Needle Lock, Memory Cue Call Lock: refused while a track plays.
        eject_lock: bool = false;
        needle_lock: bool = false;
        memory_cue_lock: bool = false;
        /// Deck > Load > Load Setting: the cue point is the memory / hot cue nearest to the start.
        load_nearest_cue: bool = false;
        /// Deck > Auto Cue and its level in dB (one of [`AUTO_CUE_DB`]).
        auto_cue: bool = true;
        auto_cue_level: i32 = -60;
        /// Deck > Vinyl Speed Adjust: steps 1..=5 into [`VINYL_SECONDS`] for Touch/Brake and Release/Start.
        vinyl_brake: u8 = 3;
        vinyl_start: u8 = 3;
        /// Deck > JOG.
        jog_speed: JogSpeed = JogSpeed::Normal;
        jog_cue_return: bool = false;
        /// Deck > Continuous Playing.
        continuous_play: bool = false;
        /// Deck > BEAT/BPM SYNC, HOT CUE gate, AUTO BEAT LOOP length in beats.
        sync_type: SyncType = SyncType::Beat;
        sync_half_double: bool = true;
        hot_cue_gate: bool = false;
        auto_loop_beats: u32 = 4;
        /// Mixer > Auto Gain, EQ, Channel Fader Curve, Output Level in dB.
        auto_gain: bool = true;
        eq_type: EqType = EqType::Eq;
        fader_curve: Fader = Fader::LINEAR;
        /// Mixer > COLOR FX KNOB: NORMAL or a plugin's knob mode (a stop the knob cannot be dragged past).
        cfx_knob: KnobMode = KnobMode::NORMAL;
        output_level: i32 = 0;
        /// Effect > RELEASE FX units and "Apply RELEASE FX on CFX"; MERGE FX.
        release_units: u8 = 1;
        release_cfx_off: bool = false;
        merge_enabled: bool = true;
        merge_stop_deck: bool = false;
        /// Sampler > SLOT count and "do not overwrite", Capture location, loop capture, Auto Panel Mode, Sequence Load.
        sampler_slots: u8 = 16;
        sampler_keep_loaded: bool = false;
        capture_dir: String = String::new();
        capture_loop_empty: bool = false;
        sampler_auto_panel: bool = false;
        sequence_load: bool = false;
        /// Sampler > SLICER: the LENGTH (bars, one of [`SLICER_BARS`]) and ROLL (1/n of it, one of [`SLICER_ROLL`]) a deck's slicer starts with.
        slicer_length: u8 = 1;
        slicer_roll: u8 = 8;
        /// Others > Mode, SNAP, QUANTIZE per function and its beat length, Setting: Q on all decks at once.
        quantize_mode: QuantizeMode = QuantizeMode::Immediate;
        snap_length: BeatLength = BeatLength::One;
        quantize_length: BeatLength = BeatLength::One;
        quantize_hot_cue: bool = true;
        quantize_loop: bool = true;
        quantize_reverse: bool = true;
        quantize_sequencer: bool = true;
        quantize_all_decks: bool = false;
        /// Others > MIDI CLOCK OUT: part of the name of the output port that gets the clock; empty = off ([`crate::app::midi`]).
        midi_clock_port: String = String::new();
        /// MIX POINT LINK > Enable, Default MIX POINT Setting and auto switch, Display time, Display grids (+ only when enlarged),
        /// Automatically changes the BEAT/BPM SYNC setting, Automatically limits the zoom out range.
        mix_point_link: bool = false;
        mix_point_default: MixPointDefault = MixPointDefault::In;
        mix_point_auto_switch: bool = false;
        mix_point_time: bool = true;
        mix_point_grids: bool = true;
        mix_point_grids_enlarged: bool = true;
        mix_point_auto_sync: bool = true;
        mix_point_zoom_limit: bool = true;
    }
);

impl Controller {
    /// Auto Cue Level, dB: a stored value that is not on the list reads as the default.
    pub fn auto_cue_db(&self) -> f32 {
        if AUTO_CUE_DB.contains(&self.auto_cue_level) {
            self.auto_cue_level as f32
        } else {
            -60.0
        }
    }

    /// Seconds a deck takes to stop (Touch/Brake: BRAKE, PAUSE, jog touch) and to get back to speed (Release/Start).
    pub fn vinyl_seconds(&self) -> (f32, f32) {
        let s = |step: u8| VINYL_SECONDS[(step.clamp(1, 5) - 1) as usize];
        (s(self.vinyl_brake), s(self.vinyl_start))
    }

    /// Track seconds per jog revolution relative to the ticks the controller reports (×0.5, ×1, ×2).
    pub fn jog_factor(&self) -> f64 {
        match self.jog_speed {
            JogSpeed::Slow => 0.5,
            JogSpeed::Normal => 1.0,
            JogSpeed::Fast => 2.0,
        }
    }

    /// AUTO BEAT LOOP length in beats; a stored value that is not on the list reads as 4.
    pub fn loop_beats(&self) -> u32 {
        if LOOP_BEATS.contains(&self.auto_loop_beats) {
            self.auto_loop_beats
        } else {
            4
        }
    }

    /// The Output Level as a linear gain (applied to the master bus).
    pub fn output_gain(&self) -> f32 {
        let db = if OUTPUT_DB.contains(&self.output_level) {
            self.output_level
        } else {
            0
        };
        10f32.powf(db as f32 / 20.0)
    }

    /// Sampler slots in use (4, 8 or 16).
    pub fn slots(&self) -> usize {
        if SAMPLER_SLOTS.contains(&self.sampler_slots) {
            self.sampler_slots as usize
        } else {
            16
        }
    }

    /// SLICER LENGTH in bars; a stored value that is not on the list reads as 1.
    pub fn slicer_bars(&self) -> u8 {
        if SLICER_BARS.contains(&self.slicer_length) {
            self.slicer_length
        } else {
            1
        }
    }

    /// SLICER ROLL denominator; a stored value that is not on the list reads as 8 (the whole slice).
    pub fn slicer_roll(&self) -> u8 {
        if SLICER_ROLL.contains(&self.slicer_roll) {
            self.slicer_roll
        } else {
            8
        }
    }

    /// RELEASE FX units: 1 applies to the master output, 2 to each Beat FX unit's own assignment.
    pub fn release_units(&self) -> u8 {
        self.release_units.clamp(1, 2)
    }

    /// Picks a numeric setting by the index of the option clicked in its drop-down.
    pub fn pick_number(&mut self, key: &str, i: usize) -> bool {
        fn at<T: Copy>(list: &[T], i: usize) -> Option<T> {
            list.get(i).copied()
        }
        match key {
            "auto_cue_level" => at(&AUTO_CUE_DB, i).map(|v| self.auto_cue_level = v),
            "vinyl_brake" => Some(self.vinyl_brake = i.clamp(0, 4) as u8 + 1),
            "vinyl_start" => Some(self.vinyl_start = i.clamp(0, 4) as u8 + 1),
            "auto_loop_beats" => at(&LOOP_BEATS, i).map(|v| self.auto_loop_beats = v),
            "output_level" => at(&OUTPUT_DB, i).map(|v| self.output_level = v),
            "release_units" => Some(self.release_units = i.clamp(0, 1) as u8 + 1),
            "sampler_slots" => at(&SAMPLER_SLOTS, i).map(|v| self.sampler_slots = v),
            "slicer_length" => at(&SLICER_BARS, i).map(|v| self.slicer_length = v),
            "slicer_roll" => at(&SLICER_ROLL, i).map(|v| self.slicer_roll = v),
            _ => return false,
        };
        true
    }
}

/// The page: the tabs one below the other.
pub fn rows(p: &Prefs) -> Vec<Row> {
    let c = &p.controller;
    let mut rows = deck::rows(p);
    rows.extend(mixer::rows(p));
    rows.extend(effect::rows(c));
    rows.extend(sampler::rows(c));
    rows.push(Row::header("RECORDINGS"));
    rows.extend(super::advanced::recording_rows(&p.advanced));
    rows.extend(mixpoint::rows(c));
    rows.extend(others::rows(c));
    rows
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    p.controller.pick_number(key, i)
        || p.controller.pick(key, i)
        || deck::choose(p, key, i)
        || mixer::choose(p, key, i)
        || super::advanced::choose(p, key, i)
}

/// Registers the text rows of this page (the sampler's capture location, the MIDI clock port).
pub fn wire() {
    super::on_text("capture_dir", |v| {
        super::update(|p| p.controller.capture_dir = v.trim().to_owned())
    });
    super::on_text("midi_clock_port", |v| {
        super::update(|p| p.controller.midi_clock_port = v.trim().to_owned())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_values_off_the_lists_read_as_the_defaults() {
        let c = Controller {
            auto_cue_level: -7,
            auto_loop_beats: 3,
            output_level: 5,
            sampler_slots: 9,
            vinyl_brake: 0,
            vinyl_start: 99,
            ..Default::default()
        };
        assert_eq!(
            (c.auto_cue_db(), c.loop_beats(), c.output_gain(), c.slots()),
            (-60.0, 4, 1.0, 16)
        );
        assert_eq!(c.vinyl_seconds(), (4.0, 0.25), "steps clamp into 1..=5");
    }

    #[test]
    fn dropdown_picks_set_the_numbers_and_ignore_other_keys() {
        let mut c = Controller::default();
        assert!(c.pick_number("auto_cue_level", 7) && c.auto_cue_level == -36);
        assert!(c.pick_number("output_level", 4) && (c.output_gain() - 0.3548).abs() < 1e-3);
        assert!(c.pick_number("vinyl_brake", 0) && c.vinyl_seconds().0 == 4.0);
        assert!(c.pick_number("sampler_slots", 0) && c.slots() == 4);
        assert!(!c.pick_number("auto_cue_level", 99) || c.auto_cue_level == -36);
        assert!(!c.pick_number("eject_lock", 1), "booleans go through pick");
        assert!(c.pick("eject_lock", 1) && c.eject_lock);
        assert!(c.pick_number("slicer_length", 3) && c.pick_number("slicer_roll", 2));
        assert_eq!((c.slicer_bars(), c.slicer_roll()), (8, 32));
        c.slicer_length = 3;
        assert_eq!(
            c.slicer_bars(),
            1,
            "a stored value off the list reads as the default"
        );
    }

    #[test]
    fn the_file_round_trips() {
        use crate::app::prefs::Section;
        let mut c = Controller {
            sync_type: SyncType::Bpm,
            auto_cue_level: -42,
            capture_dir: "/tmp/x y".into(),
            quantize_length: BeatLength::Quarter,
            ..Default::default()
        };
        c.eject_lock = true;
        let mut text = Vec::new();
        c.write(&mut |k, v| text.push((k.to_owned(), v.to_owned())));
        let mut back = Controller::default();
        for (k, v) in &text {
            assert_eq!(back.parse(k, v), Some(true), "{k}");
        }
        assert_eq!(back, c);
    }

    #[test]
    fn every_section_survives_the_settings_file() {
        let mut p = Prefs::default();
        p.controller.sync_type = SyncType::Bpm;
        p.controller.output_level = -12;
        p.advanced.rec_threshold = -45;
        p.advanced.rec_location = "/tmp/recs".into();
        p.extensions.stems_layout = crate::app::prefs::extensions::StemsLayout::Custom;
        p.extensions.lighting = true;
        p.keyboard.keymap = "preset=default;deck.cue.A=q".into();
        let (back, notes) = Prefs::parse(&p.to_text());
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(back, p);
    }

    #[test]
    fn the_page_lists_every_tab_and_each_click_key_is_handled() {
        let p = Prefs::default();
        let rows = rows(&p);
        let headers: Vec<_> = rows
            .iter()
            .filter(|r| r.kind == crate::app::prefs::rows::Kind::Header)
            .map(|r| r.label.as_str())
            .collect();
        for tab in [
            "DECK",
            "MIXER",
            "EFFECT",
            "SAMPLER",
            "RECORDINGS",
            "MIX POINT LINK",
            "OTHERS",
        ] {
            assert!(
                headers.iter().any(|h| h.starts_with(tab)),
                "{tab} in {headers:?}"
            );
        }
        for r in rows
            .iter()
            .filter(|r| !r.key.is_empty() && r.kind != crate::app::prefs::rows::Kind::Info)
        {
            let mut q = Prefs::default();
            assert!(
                choose(&mut q, &r.key, 0)
                    || super::super::advanced::choose(&mut q, &r.key, 0)
                    || q.choose(&r.key, 0),
                "{} is not handled",
                r.key
            );
        }
    }
}
