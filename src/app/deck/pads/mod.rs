//! Performance pads, twelve modes and the plugin modes (`DeckView.pad_mode`, in the order of the mode buttons in
//! ui/deck-panel.slint, plugin modes after the built-in ones, see [`plugin`]):
//! HOT CUE A to H, BEAT JUMP (-1 +1 -2 +2 -4 +4 -8 +8 beats), KEYBOARD (pad = manual key shift
//! -3 to +4 semitones, the current one lit, the key is shifted rather
//! than the cue played chromatically), BEAT LOOP (1/8 to 16 beats), PAD FX (momentary: a held pad runs one Beat FX
//! effect and beat on the deck's channel through the deck's own PAD FX unit, release = off; 16 effects, A to H and
//! the SHIFT page I to P, see [`PAD_FX`]), KEY SHIFT
//! (+4 +5 +6 +7 0 +1 +2 +3 semitones, the controller defaults), SAMPLE SCRATCH (pad n loads
//! sampler slot n's sample onto the deck as a track, so the jog scratches it; empty slots are dark and do
//! nothing; on a controller SHIFT + pad unloads the track, [`Deck::unload`],),
//! SLICER / SLICE LOOP ([`slicer`](super::slicer)), SAMPLER and SEQ. CALL
//! ([`sequencer`](super::sequencer)); their option chips are in [`padopts`](super::padopts). STEMS is the last
//! built-in (it only exists while PREFS has it on).
//!
//! Children: [`layout`] builds the 8 pad views of a mode (pure, ui/pads.slint draws them); [`cues`] sets and
//! deletes hot and memory cues from the pads. Cues the user sets or deletes edit the in-memory `Analysis` and
//! are saved to the store ([`cuelist`](super::cuelist)); the library's files are never written, a reload
//! shows the stored cues.

mod cues;
mod layout;
mod plugin;

use super::{Deck, Loaded, beats_between, grid_bpm};
use crate::app::{
    PadView,
    fx::pad_fx_rgb,
    prefs,
    prefs::view::{PadFxColor, SamplerColor},
    sampler,
};
use openfader::analysis::{Analysis, Cue};
use openfader::audio::{
    self,
    fx::{BEAT_TEXT, Effect},
};
use openfader::library::Track;
use slint::{Color, Model, ModelRc, VecModel};

pub(super) use layout::{cue_pads, views};

/// Declaration order is the mode buttons' order (`mode as i32`, [`PadMode::ALL`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PadMode {
    HotCue,
    BeatJump,
    Keyboard,
    BeatLoop,
    PadFx,
    KeyShift,
    SampleScratch,
    /// SLICER: a held pad rolls one of the 8 slices of the range; SLICE LOOP: the range loops meanwhile.
    Slicer,
    SlicerLoop,
    /// SAMPLER: the pads play the slots of the sampler's bank; SEQ. CALL: they play the saved sequences.
    Sampler,
    SeqCall,
    /// STEMS: pads 1 to 4 are the parts VOCAL, INST, BASS, DRUMS (ACTIVE PART: mute or solo per PREFS), lit
    /// while heard; pads 5 to 8 are STEM INSTANT DOUBLES of the same parts ([`Deck::stem_double`]).
    Stems,
    /// A plugin's pad mode ([`plugin`]): the mode index minus [`PadMode::ALL`]`.len()` selects the declaration.
    Plugin,
}

impl PadMode {
    /// The built-in modes; a plugin mode's index continues after them (the order of the declarations in the registry).
    const ALL: [Self; 12] = [
        Self::HotCue,
        Self::BeatJump,
        Self::Keyboard,
        Self::BeatLoop,
        Self::PadFx,
        Self::KeyShift,
        Self::SampleScratch,
        Self::Slicer,
        Self::SlicerLoop,
        Self::Sampler,
        Self::SeqCall,
        Self::Stems,
    ];
    /// Mode button labels, in the order of [`PadMode::ALL`].
    const NAMES: [&'static str; 12] = [
        "HOT CUE",
        "JUMP",
        "KEYBOARD",
        "LOOP",
        "PAD FX",
        "KEY SHIFT",
        "S.SCRATCH",
        "SLICER",
        "SLICE LOOP",
        "SAMPLER",
        "SEQ. CALL",
        "STEMS",
    ];

    /// Mode indices a controller's mode cycle wraps at: the built-ins and the plugin modes. STEMS keeps its index
    /// while PREFS has it off; [`Deck::set_pad_mode`] steps over it then.
    pub fn count() -> i32 {
        Self::count_in(plugin::count())
    }

    fn count_in(plugins: usize) -> i32 {
        (Self::ALL.len() + plugins) as i32
    }

    /// Unknown index: HOT CUE.
    pub fn from_index(i: i32) -> Self {
        Self::from_index_in(i, plugin::count())
    }

    fn from_index_in(i: i32, plugins: usize) -> Self {
        match usize::try_from(i) {
            Ok(i) if i < Self::ALL.len() => Self::ALL[i],
            Ok(i) if i < Self::ALL.len() + plugins => Self::Plugin,
            _ => Self::HotCue,
        }
    }

    /// SLICER or SLICE LOOP.
    pub fn is_slicer(self) -> bool {
        matches!(self, Self::Slicer | Self::SlicerLoop)
    }
}

/// The mode button labels for the selector: the built-ins (STEMS only with `stems`), then the plugin modes.
pub(super) fn mode_names(stems: bool) -> ModelRc<slint::SharedString> {
    let built_in = &PadMode::NAMES[..PadMode::NAMES.len() - usize::from(!stems)];
    let names = built_in.iter().copied().chain(plugin::names());
    ModelRc::new(VecModel::from(names.map(Into::into).collect::<Vec<_>>()))
}

/// KEY SHIFT pads: semitones above the track's own key (pad 5 = 0), the controller defaults.
const KEY_SHIFTS: [i32; 8] = [4, 5, 6, 7, 0, 1, 2, 3];
/// PAD FX pads: label, effect and index into [`BEAT_TEXT`]. The slots are our choice (effects A to H), built from the Beat FX effects. Slots 8 to 15 are the
/// SHIFT page, effects I to P; the controller's SHIFT + pad reaches them, page 1 of [`page_range`].
const PAD_FX: [(&str, Effect, usize); 16] = [
    ("ECHO", Effect::Echo, 3),
    ("DLY", Effect::Delay, 4),
    ("REV", Effect::Reverb, 5),
    ("FLNG", Effect::Flanger, 7),
    ("PHSR", Effect::Phaser, 7),
    ("ROLL", Effect::Roll, 3),
    ("TRNS", Effect::Trans, 3),
    ("HPF", Effect::Hpf, 6),
    ("LPF", Effect::Lpf, 5),
    ("ECHO", Effect::Echo, 5),
    ("DLY", Effect::Delay, 2),
    ("REV", Effect::Reverb, 7),
    ("FLNG", Effect::Flanger, 5),
    ("PHSR", Effect::Phaser, 5),
    ("ROLL", Effect::Roll, 2),
    ("TRNS", Effect::Trans, 2),
];
/// LEVEL / DEPTH of the PAD FX unit while a pad is held.
const PAD_FX_LEVEL: f32 = 0.8;
const JUMPS: [i32; 8] = [-1, 1, -2, 2, -4, 4, -8, 8];
const LOOPS: [(f64, &str); 8] = [
    (0.125, "1/8"),
    (0.25, "1/4"),
    (0.5, "1/2"),
    (1.0, "1"),
    (2.0, "2"),
    (4.0, "4"),
    (8.0, "8"),
    (16.0, "16"),
];

/// Manual key shift of keyboard pad `pad`: -3 to +4 (pad 3 = the track's own key).
const fn keyboard_shift(pad: usize) -> i32 {
    pad as i32 - 3
}

/// Pad pages: PAD FX page 1 is its SHIFT page (effects I to P); BEAT JUMP page `p` multiplies the sizes by 4^p (SHIFT + pad
/// 7 / 8 on a controller: fewer / more beats); KEYBOARD and KEY SHIFT
/// move all eight semitones by one per page (SHIFT + pad 7 up / pad 8 down; the limit is the key shift's
/// own). Returns the (lowest, highest) page of `mode`.
const fn page_range(mode: PadMode) -> (i32, i32) {
    match mode {
        PadMode::PadFx => (0, 1),
        PadMode::BeatJump => (0, 2),
        PadMode::Keyboard | PadMode::KeyShift => (-12, 12),
        _ => (0, 0),
    }
}

/// Beats pad `pad` jumps on `page`.
const fn page_jump(pad: usize, page: i32) -> i32 {
    JUMPS[pad] << (2 * page)
}

/// Semitones of keyboard / key-shift pad `pad` (`table` = the page-0 shifts) on `page`.
const fn page_shift(table: i32, page: i32) -> i32 {
    table + page
}

/// Hot cue of pad `pad` (0-based): hot cue number `pad + 1`.
fn hot(a: &Analysis, pad: usize) -> Option<&Cue> {
    a.cues.iter().find(|c| c.hot_cue as usize == pad + 1)
}

impl Deck {
    /// PAD FX pad `pad` down: this deck's own PAD FX unit (fixed on its channel, see `audio::Bus`) takes the
    /// pad's effect and beat at this deck's tempo; the Beat FX panel's unit and the other deck's are untouched.
    fn pad_fx_on(&self, pad: usize) {
        let (name, effect, beat) = PAD_FX[pad];
        let bpm = self
            .with_analysis(|a, _| grid_bpm(&a.beats))
            .map_or(0.0, |g| (g * self.rate()) as f32);
        let index = self.index;
        self.with_player(|p| {
            let c = &p.bus().pad_fx[index];
            c.set_effect(effect);
            c.set_beat(beat);
            c.set_level(PAD_FX_LEVEL);
            c.set_bpm(bpm);
            c.set_on(true);
        });
        self.pad_fx.set(Some(pad));
        self.refresh_pads();
        self.say(format!("pad fx {name} {}", BEAT_TEXT[beat]));
    }

    /// Releases the held PAD FX pad (whatever the mode now is); the effect fades out, echo / reverb tails ring out.
    pub(super) fn pad_fx_off(&self) {
        if self.pad_fx.take().is_some() {
            self.with_player(|p| p.bus().pad_fx[self.index].set_on(false));
            self.refresh_pads();
        }
    }

    /// The PAD FX pad held, 0..16 (8..16 = the SHIFT page): the controller's pad lights follow it.
    pub(in crate::app) fn pad_fx_held(&self) -> Option<usize> {
        self.pad_fx.get()
    }

    pub(super) fn pad_mode(&self) -> PadMode {
        PadMode::from_index(self.view.borrow().pad_mode)
    }

    /// Pads of the current mode for `l` (loaded state, so callers holding it borrowed can use it).
    pub(super) fn pad_views(&self, l: &Loaded) -> Vec<PadView> {
        let mode = self.pad_mode();
        if let (PadMode::HotCue, Some(b)) = (mode, self.bank.borrow().as_ref()) {
            return super::bank::views(b);
        }
        let mut v = layout::mode_views(
            mode,
            &l.analysis,
            self.key_st.get(),
            l.loop_.and_then(|lp| beats_between(&l.analysis.beats, lp)),
            self.pad_page.get(),
        );
        match mode {
            PadMode::PadFx => {
                // PAD FX pads take the colour of VIEW > Color
                let [r, g, b] = pad_fx_rgb(prefs::with(|p| p.view.pad_fx_color));
                v.iter_mut()
                    .for_each(|p| p.color = Color::from_rgb_u8(r, g, b));
                // the held pad, when its page is the one shown
                if let Some(held) = self
                    .pad_fx
                    .get()
                    .filter(|h| h / 8 == self.pad_page.get() as usize)
                {
                    v[held % 8].set = true;
                }
            }
            PadMode::SampleScratch => {
                // Only the first SLOT-count slots of PREFS exist; SAMPLER colour may be one fixed colour.
                let bus = self.with_player(|p| p.bus());
                let (slots, purple) = prefs::with(|p| {
                    (
                        p.controller.slots(),
                        p.view.sampler_color == SamplerColor::Purple,
                    )
                });
                let [r, g, b] = pad_fx_rgb(PadFxColor::Purple);
                v.iter_mut().enumerate().for_each(|(i, pad)| {
                    pad.set =
                        i < slots && bus.as_ref().is_some_and(|b| b.sampler.slice(i).is_some());
                    if purple {
                        pad.color = Color::from_rgb_u8(r, g, b);
                    }
                });
            }
            PadMode::Slicer | PadMode::SlicerLoop => {
                if let Some(i) = self.slicer_lit(l) {
                    v[i].set = true;
                }
            }
            PadMode::Sampler => self.sampler_pads(&mut v),
            PadMode::SeqCall => self.seqcall_pads(&mut v),
            PadMode::Stems => {
                let (lit, four) = (self.stem_pads_lit(), prefs::with(|p| p.stems_four));
                v.iter_mut().enumerate().for_each(|(i, p)| p.set = lit[i]);
                if !four {
                    v[audio::BASS].label = "".into(); // 3 stems: bass is inside INST
                    v[audio::STEM_COUNT + audio::BASS].label = "".into();
                }
            }
            _ => {}
        }
        v
    }

    pub(in crate::app) fn refresh_pads(&self) {
        let pads = self.pad_views(&self.loaded.borrow());
        self.show(|v| v.pads = ModelRc::new(VecModel::from(pads)));
        self.refresh_options();
    }

    /// Mode button pressed (index in the order of [`PadMode`], wrapped: a mode cycle may pass `mode + 1`);
    /// leaves delete mode and releases a held PAD FX pad.
    pub fn set_pad_mode(&self, mode: i32) {
        self.enter_mode(mode, false);
    }

    /// [`Deck::set_pad_mode`]; `keep_page`: when `mode` is the one already shown its page stays (a controller pad press).
    fn enter_mode(&self, mode: i32, keep_page: bool) {
        let mut mode = mode.rem_euclid(PadMode::count());
        if mode == PadMode::Stems as i32 && !prefs::with(|p| p.stems) {
            mode = (mode + 1).rem_euclid(PadMode::count()); // STEMS is off: the cycle goes on to the plugin modes or HOT CUE
        }
        let keep = keep_page && self.view.borrow().pad_mode == mode;
        self.slicer_switch(self.pad_mode(), PadMode::from_index(mode));
        self.sampler_release();
        self.pad_fx_off();
        if !keep {
            self.pad_page.set(0);
        }
        self.show(|v| {
            v.pad_mode = mode;
            v.delete_mode = false;
        });
        self.refresh_pads();
    }

    /// A pad of the controller's own layer: pad `slot` of `mode`, 0..8, or 8..16 for PAD FX's SHIFT page (effects I to P,
    /// the page is then shown). The page the user chose with the page keys stays.
    pub fn pad_hw(&self, mode: i32, slot: usize) {
        self.enter_mode(mode, true);
        if self.pad_mode() == PadMode::PadFx {
            self.pad_page.set((slot / 8) as i32);
            self.refresh_pads();
        }
        self.pad(slot % 8);
    }

    /// Pad pressed: what it does depends on the mode (see the module doc). PAD FX stays on until
    /// [`Deck::pad_released`].
    pub fn pad(&self, pad: usize) {
        if pad >= 8 {
            return;
        }
        let mode = self.pad_mode();
        if mode == PadMode::HotCue && self.bank_pad(pad) {
            return;
        }
        match mode {
            PadMode::SampleScratch => return self.sample_scratch(pad), // works on an empty deck too
            PadMode::Sampler => return self.sampler_pad(pad),
            PadMode::SeqCall => return sampler::call(pad),
            _ => {}
        }
        if !self.ready() {
            return;
        }
        let page = self.pad_page.get();
        match mode {
            PadMode::PadFx => self.pad_fx_on(pad + 8 * page as usize),
            PadMode::KeyShift => self.set_key_shift(page_shift(KEY_SHIFTS[pad], page)),
            PadMode::HotCue => self.hot_pad(pad),
            PadMode::BeatJump => self.beat_jump(page_jump(pad, page)),
            PadMode::Keyboard => self.set_key_shift(page_shift(keyboard_shift(pad), page)),
            PadMode::BeatLoop => self.beat_loop(LOOPS[pad].0),
            PadMode::SampleScratch | PadMode::Sampler | PadMode::SeqCall => {}
            PadMode::Plugin => self.plugin_pad(pad),
            PadMode::Slicer | PadMode::SlicerLoop => self.slicer_pad(pad),
            PadMode::Stems if pad >= audio::STEM_COUNT => self.stem_double(pad - audio::STEM_COUNT),
            PadMode::Stems => self.stem_press(pad),
        }
    }

    /// Page button (shift + pad 7 / 8 of a pad mode on a controller): the previous (`dir < 0`) or next page of the
    /// current mode's pads; stops at the ends, modes without pages ignore it.
    pub fn pad_page_step(&self, dir: i32) {
        let (lo, hi) = page_range(self.pad_mode());
        self.pad_page
            .set((self.pad_page.get() + dir.signum()).clamp(lo, hi));
        self.refresh_pads();
    }

    /// SAMPLE SCRATCH pad `pad`: sampler slot `pad`'s sample becomes this deck's track (paused at its start, like a
    /// fresh load) and the jog scratches it; an empty slot says so and changes nothing.
    fn sample_scratch(&self, slot: usize) {
        if self.eject_locked() {
            return;
        }
        let slots = prefs::with(|p| p.controller.slots());
        if slot >= slots {
            return self.say(format!(
                "sampler slot {} is not in use ({slots} slots)",
                slot + 1
            ));
        }
        let Some(pcm) = self.with_player(|p| p.bus().sampler.slice(slot)).flatten() else {
            return self.say(format!(
                "sampler slot {} is empty (fill it in the sampler panel)",
                slot + 1
            ));
        };
        let t = Track {
            title: format!("Sampler slot {}", slot + 1),
            length_s: pcm.duration_ms() as i64 / 1000,
            ..Default::default()
        };
        self.install(&t, Analysis::default(), vec![], Some((*pcm).clone()));
        self.poll_decode();
    }

    /// SHIFT + SAMPLE SCRATCH pad: the deck's track is unloaded and the
    /// deck is empty as before its first LOAD (the load lock applies: a playing deck keeps its track).
    pub fn unload(&self) {
        if self.eject_locked() {
            return;
        }
        self.install(
            &Track::default(),
            Analysis::default(),
            vec![],
            Some(audio::Pcm::default()),
        );
        *self.loaded.borrow_mut() = Loaded::default();
        self.with_player(|p| p.pause());
        self.refresh_pads();
        // tick() leaves a deck that is not ready alone, so what it drew last is cleared here
        self.show(|v| {
            (v.loaded, v.loading, v.playing, v.at_cue, v.ending) =
                (false, false, false, false, false);
            (v.detail, v.overview) = Default::default();
            v.status = "empty: select a track, then LOAD".into();
        });
        self.scroll.set_row_data(self.index, Default::default());
        openfader::plugin::event::fire(
            "track_unloaded",
            serde_json::json!({"deck": self.index + 1}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_index_round_trips_and_unknown_is_hot_cue() {
        for i in 0..PadMode::ALL.len() as i32 {
            assert_eq!(PadMode::from_index_in(i, 0) as i32, i);
        }
        let n = PadMode::ALL.len() as i32;
        assert_eq!(PadMode::from_index_in(n, 0), PadMode::HotCue);
        assert_eq!(PadMode::from_index_in(n, 1), PadMode::Plugin);
        assert_eq!(PadMode::from_index_in(-1, 0), PadMode::HotCue);
    }

    #[test]
    fn pages_scale_beat_jumps_and_stop_at_the_ends() {
        assert_eq!(page_jump(7, 1), 32);
        assert_eq!(page_jump(0, 2), -16);
        assert_eq!(
            [page_range(PadMode::BeatJump), page_range(PadMode::HotCue)],
            [(0, 2), (0, 0)]
        );
    }

    #[test]
    fn keyboard_pads_span_minus_3_to_plus_4() {
        assert_eq!(
            (0..8).map(keyboard_shift).collect::<Vec<_>>(),
            [-3, -2, -1, 0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn pad_fx_beats_index_the_beat_text() {
        assert!(PAD_FX.iter().all(|&(_, _, beat)| beat < BEAT_TEXT.len()));
    }
}
