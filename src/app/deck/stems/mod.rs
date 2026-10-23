//! STEMS on a deck: the PREFS switch starts a background separation
//! ([`openfader::stems::Job`]) for each loaded library track, shown as a progress text in the stem row. The stems go to
//! the player ([`openfader::audio::Player::set_stems`]) as soon as the job starts and fill in while the track plays: frames not
//! separated yet play as the original track. SPEED separates the play position and the HOT CUEs first, QUALITY goes from the start of the track
//! ([`openfader::stems::schedule`]). The VOCAL / INST / BASS / DRUMS buttons (pills filled while their part plays at full
//! level, outlined below it; clickable as soon as a track is loaded), the STEMS pad mode, the controller's ACTIVE PART buttons
//! and the mixer's STEM-mode knobs drive
//! per-stem gains, ramped in the audio thread, so nothing clicks and key lock, scratch, loops and slip carry on.
//!
//! Gain of a stem = its mute state × its isolator knob. A deck button silences a part at full level and brings any other
//! level back to full. The pads and the controller's ACTIVE PART follow PREFS (MUTE: the same as the buttons; SOLO: silence
//! every other part, pressing the sole part again brings all back); either way the part's rotary moves with it, and turning a
//! rotary up brings back the parts it drives. A part button only mutes or solos: the mixer strip stays in its EQ or STEM ISO mode.
//! The mixer has three knobs, top to bottom INST, VOCAL, DRUMS; BASS rides the INST knob. With 3 STEMS
//! (PREFS) bass is part of INST: it follows INST's mute and has no button.
//! The mute state resets on LOAD unless PREFS says KEEP. Without the model the first separation downloads it
//! ([`openfader::stems::model`]); offline, the stem row says so and the status line names the command to fetch it by hand.
//! The waveforms follow the gains when PREFS STEM waveform is on: see [`super::stemwave`].
//!
//! This file holds the gain maths, the per-deck state and the button side; `job` runs the separation.

mod job;

use super::Deck;
use crate::app::prefs;
use openfader::audio::{BASS, DRUMS, INST, STEM_COUNT, VOCAL};
use openfader::stems::Job;
use slint::{ComponentHandle, ModelRc, VecModel};

/// Player gains of VOCAL, INST, BASS, DRUMS from the mute buttons and the three isolator knobs (VOCAL, INST, DRUMS; 0..=1; the
/// mixer shows them as INST, VOCAL, DRUMS, see `mixer::channel`).
pub fn gains(mute: [bool; STEM_COUNT], iso: [f32; 3], four: bool) -> [f32; STEM_COUNT] {
    let on = |k: usize| if mute[k] { 0.0 } else { 1.0 };
    let bass = if four { on(BASS) } else { on(INST) };
    let mut g = [0.0; STEM_COUNT];
    g[VOCAL] = on(VOCAL) * iso[0];
    g[INST] = on(INST) * iso[1];
    g[BASS] = bass * iso[1];
    g[DRUMS] = on(DRUMS) * iso[2];
    g
}

/// ACTIVE PART press on stem `k`. `solo`: silence every other part, or when `k` is already the only one heard, bring all back;
/// else toggle `k`. Bass is not a part of its own with 3 stems (`four` false): pressing it does nothing.
pub fn press(mute: &mut [bool; STEM_COUNT], k: usize, solo: bool, four: bool) {
    if k >= STEM_COUNT || (k == BASS && !four) {
        return;
    }
    if !solo {
        mute[k] = !mute[k];
        return;
    }
    let others = || (0..STEM_COUNT).filter(move |&j| j != k && (four || j != BASS));
    let alone = !mute[k] && others().all(|j| mute[j]);
    for j in others() {
        mute[j] = !alone;
    }
    mute[k] = false;
}

/// A stem at this gain or above counts as full: its button is filled, a press silences it.
const FULL: f32 = 0.99;

/// The mixer rotary (0 VOCAL, 1 INST, 2 DRUMS) that drives stem `k`; BASS rides INST's.
fn part_of(k: usize) -> usize {
    match k {
        VOCAL => 0,
        INST | BASS => 1,
        _ => 2,
    }
}

/// Button gain endpoints; INST and BASS share one rotary without muting each other. Without `solo` a press silences a part at
/// full level and brings any other level (silenced or turned down on the mixer) back to full.
fn activate(
    mute: &mut [bool; STEM_COUNT],
    iso: [f32; 3],
    k: usize,
    solo: bool,
    four: bool,
) -> [f32; 3] {
    let before = gains(*mute, iso, four);
    *mute = before.map(|g| g == 0.0);
    if solo {
        press(mute, k, solo, four);
    } else if k < STEM_COUNT && (four || k != BASS) {
        mute[k] = before[k] >= FULL;
    }
    let audible = [
        !mute[VOCAL],
        !mute[INST] || (four && !mute[BASS]),
        !mute[DRUMS],
    ];
    let selected = part_of(k);
    std::array::from_fn(|part| {
        if solo || part == selected {
            if audible[part] { 1.0 } else { 0.0 }
        } else {
            iso[part]
        }
    })
}

/// Mixer knob `p` (VOCAL, INST, DRUMS) moved from `old` to a level above zero while every part it drives is silenced: those
/// parts come back, so the knob sets what is heard.
fn knob_moved(mute: &mut [bool; STEM_COUNT], old: [f32; 3], new: [f32; 3], four: bool) {
    let inst: &[usize] = if four { &[INST, BASS] } else { &[INST] };
    for (p, parts) in [&[VOCAL][..], inst, &[DRUMS]].into_iter().enumerate() {
        if new[p] > 0.0 && new[p] != old[p] && parts.iter().all(|&k| mute[k]) {
            parts.iter().for_each(|&k| mute[k] = false);
        }
    }
}

pub(super) struct State {
    job: Option<Job>,
    attached: Option<std::sync::Arc<openfader::audio::Stems>>,
    /// Stems are attached to the player and the buttons work (the separation may still be running).
    ready: bool,
    /// The separation finished: every frame has its stems (the stem waveform and INSTANT DOUBLES wait for it).
    complete: bool,
    /// Muted by the buttons / pads / controller (true = silent).
    mute: [bool; STEM_COUNT],
    /// The mixer's isolator knobs: VOCAL, INST, DRUMS.
    iso: [f32; 3],
    /// Progress, readiness or error text shown beside the buttons.
    text: String,
    /// (STEMS, 4 stems, SOLO, ACTIVE STEM buttons shown, SPEED) as last applied, to ignore unrelated settings changes.
    seen: Option<(bool, bool, bool, bool, bool)>,
    /// (play position's segment, hot cues) the job was last pointed at.
    focus: Option<(usize, usize)>,
}

impl Default for State {
    fn default() -> Self {
        State {
            job: None,
            attached: None,
            ready: false,
            complete: false,
            mute: [false; STEM_COUNT],
            iso: [1.0; 3],
            text: String::new(),
            seen: None,
            focus: None,
        }
    }
}

impl Deck {
    pub(in crate::app) fn show_stems(&self) {
        let (s, swapped) = (self.stems.borrow(), self.drums_swapped());
        let (stems, four, buttons) =
            prefs::with(|p| (p.stems, p.stems_four, p.extensions.stem_displays()[0]));
        let effective = gains(s.mute, s.iso, four);
        let full: Vec<bool> = effective
            .iter()
            .enumerate()
            .map(|(k, g)| *g >= FULL && !(swapped && k == DRUMS))
            .collect();
        let (ready, text) = (s.ready, s.text.clone());
        drop(s);
        self.show(|v| {
            v.stems = stems;
            v.stem_four = four;
            v.stem_buttons = buttons;
            v.stem_ready = ready;
            v.stem_full = ModelRc::new(VecModel::from(full));
            v.stem_text = text.into();
        });
    }

    /// Pushes the gains to the player (stems or not: the player keeps them for when stems arrive).
    pub(super) fn apply_stems(&self) {
        let g = self.gains_now();
        self.with_player(|p| {
            g.iter()
                .enumerate()
                .for_each(|(k, &g)| p.set_stem_gain(k, g))
        });
    }

    fn gains_now(&self) -> [f32; STEM_COUNT] {
        self.stem_gains_for(prefs::with(|p| p.stems_four))
    }

    fn stem_gains_for(&self, four: bool) -> [f32; STEM_COUNT] {
        let s = self.stems.borrow();
        gains(s.mute, s.iso, four)
    }

    /// The gains the player mixes with, once the stems are ready (else `None`: the original track is heard).
    /// `four` is PREFS' 4 stems, passed in because the per-tick caller already holds the settings.
    pub(super) fn stem_gains(&self, four: bool) -> Option<[f32; STEM_COUNT]> {
        self.stems.borrow().ready.then(|| self.stem_gains_for(four))
    }

    /// The mute state (true = silent) of VOCAL, INST, BASS, DRUMS.
    pub(super) fn stem_mutes(&self) -> [bool; STEM_COUNT] {
        self.stems.borrow().mute
    }

    /// Sets the mute state and shows / applies it (INSTANT DOUBLES).
    pub(super) fn set_stem_mutes(&self, mute: [bool; STEM_COUNT]) {
        self.stems.borrow_mut().mute = mute;
        self.refresh_stem_buttons();
    }

    /// A new track replaces the old: drops its separation and, unless PREFS says KEEP, the mute state.
    pub(super) fn stems_clear(&self) {
        self.double.borrow_mut().take(); // a pending INSTANT DOUBLES belongs to the track that is going
        {
            let mut s = self.stems.borrow_mut();
            s.job = None;
            s.attached = None;
            (s.ready, s.complete, s.focus) = (false, false, None);
            s.text.clear();
            if prefs::with(|p| p.stems_reset) {
                s.mute = [false; STEM_COUNT];
            }
        }
        self.stemwave_clear();
        self.apply_stems();
        self.show_stems();
    }

    /// ACTIVE PART of the pads and the controller: starts separation on demand and switches the selected part to zero/full
    /// gain, MUTE or SOLO as PREFS says; the mixer rotaries follow the resulting part levels.
    pub fn stem_press(&self, k: usize) {
        self.stem_switch(k, prefs::with(|p| p.active_part_solo));
    }

    /// A deck's part button: a part at full level is silenced, any other level goes back to full (never a solo).
    pub fn stem_click(&self, k: usize) {
        self.stem_switch(k, false);
    }

    fn stem_switch(&self, k: usize, solo: bool) {
        let four = prefs::with(|p| p.stems_four);
        if !self.ready() || k >= STEM_COUNT || (k == BASS && !four) {
            return;
        }
        if !prefs::with(|p| p.stems) {
            prefs::update(|p| p.stems = true);
        }
        self.ensure_stems();
        let (levels, selected) = {
            let mut s = self.stems.borrow_mut();
            let iso = s.iso;
            (activate(&mut s.mute, iso, k, solo, four), part_of(k))
        };
        if let Some(app) = self.app.upgrade() {
            let mixer = app.global::<crate::app::MixerState>();
            // The rotaries the press decided (all of them for a solo); the strip keeps its EQ / STEM ISO mode.
            for (part, level) in levels.into_iter().enumerate() {
                if solo || part == selected {
                    mixer.invoke_iso_part(self.index as i32, part as i32, level);
                }
            }
        }
        self.refresh_stem_buttons();
    }

    fn refresh_stem_buttons(&self) {
        self.apply_stems();
        self.show_stems();
        self.refresh_pads();
    }

    /// The mixer's isolator knobs (VOCAL, INST, DRUMS, 0..=1; `None` while the channel is in EQ mode: all at 1, and a part
    /// silenced by a button stays silent).
    pub(in crate::app) fn set_isolators(&self, knobs: Option<[f32; 3]>) {
        let iso = knobs.unwrap_or([1.0; 3]);
        let mut s = self.stems.borrow_mut();
        if s.iso != iso {
            let (old, mute) = (s.iso, s.mute);
            s.iso = iso;
            if knobs.is_some() {
                knob_moved(&mut s.mute, old, iso, prefs::with(|p| p.stems_four));
            }
            let unmuted = s.mute != mute;
            drop(s);
            if unmuted {
                self.refresh_stem_buttons();
            } else {
                self.apply_stems();
                self.show_stems();
            }
        }
    }

    /// Pads of the STEMS mode lit for the parts heard.
    pub(super) fn stem_pads_lit(&self) -> [bool; 8] {
        let (s, swapped) = (self.stems.borrow(), self.drums_swapped());
        let four = prefs::with(|p| p.stems_four);
        std::array::from_fn(|i| {
            s.ready
                && i < STEM_COUNT
                && (four || i != BASS)
                && !s.mute[i]
                && !(swapped && i == DRUMS)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [bool; STEM_COUNT] = [false; STEM_COUNT];

    #[test]
    fn mute_toggles_and_gains_follow_mute_and_isolators() {
        let mut m = ALL;
        press(&mut m, DRUMS, false, true);
        assert_eq!(m, [false, false, false, true]);
        assert_eq!(gains(m, [1.0; 3], true), [1.0, 1.0, 1.0, 0.0]);
        press(&mut m, DRUMS, false, true);
        assert_eq!(m, ALL);
        // isolator knobs scale; BASS rides INST's knob
        assert_eq!(gains(ALL, [0.5, 0.25, 0.0], true), [0.5, 0.25, 0.25, 0.0]);
    }

    #[test]
    fn buttons_restore_zeroed_rotaries_without_changing_other_levels() {
        let mut mute = ALL;
        let iso = activate(&mut mute, [0.0, 0.4, 0.7], VOCAL, false, true);
        assert_eq!(iso, [1.0, 0.4, 0.7]);
        assert_eq!(gains(mute, iso, true), [1.0, 0.4, 0.4, 0.7]);
        let iso = activate(&mut mute, iso, VOCAL, false, true);
        assert_eq!(iso, [0.0, 0.4, 0.7]);
        assert_eq!(gains(mute, iso, true), [0.0, 0.4, 0.4, 0.7]);
        let iso = activate(&mut mute, iso, BASS, true, true);
        assert_eq!(gains(mute, iso, true), [0.0, 0.0, 1.0, 0.0]);
        let iso = activate(&mut mute, iso, BASS, true, true);
        assert_eq!(gains(mute, iso, true), [1.0; STEM_COUNT]);
    }

    #[test]
    fn a_press_silences_a_full_part_and_brings_a_turned_down_one_to_full() {
        let mut mute = ALL;
        let iso = activate(&mut mute, [1.0, 0.5, 1.0], DRUMS, false, true);
        assert_eq!(
            gains(mute, iso, true),
            [1.0, 0.5, 0.5, 0.0],
            "full DRUMS silenced"
        );
        let iso = activate(&mut mute, iso, INST, false, true);
        assert_eq!(
            gains(mute, iso, true),
            [1.0, 1.0, 1.0, 0.0],
            "INST at half goes to full"
        );
        let iso = activate(&mut mute, iso, BASS, false, true);
        assert_eq!(
            gains(mute, iso, true),
            [1.0, 1.0, 0.0, 0.0],
            "BASS alone, INST stays heard"
        );
    }

    #[test]
    fn turning_up_a_knob_brings_back_the_parts_a_button_silenced() {
        let mut mute = ALL;
        let iso = activate(&mut mute, [1.0; 3], DRUMS, false, true);
        knob_moved(&mut mute, iso, [1.0, 1.0, 0.5], true);
        assert_eq!(
            gains(mute, [1.0, 1.0, 0.5], true),
            [1.0, 1.0, 1.0, 0.5],
            "DRUMS at half heard"
        );
        // BASS silenced alone: INST's knob still drives INST, BASS stays silent
        let iso = activate(&mut mute, [1.0, 1.0, 0.5], BASS, false, true);
        knob_moved(&mut mute, iso, [1.0, 0.5, 0.5], true);
        assert_eq!(gains(mute, [1.0, 0.5, 0.5], true), [1.0, 0.5, 0.0, 0.5]);
    }

    #[test]
    fn solo_isolates_a_part_and_pressing_it_again_restores_all() {
        let mut m = ALL;
        press(&mut m, VOCAL, true, true);
        assert_eq!(gains(m, [1.0; 3], true), [1.0, 0.0, 0.0, 0.0]);
        press(&mut m, DRUMS, true, true); // switch the solo to another part
        assert_eq!(gains(m, [1.0; 3], true), [0.0, 0.0, 0.0, 1.0]);
        press(&mut m, DRUMS, true, true);
        assert_eq!(m, ALL);
    }

    #[test]
    fn three_stems_merge_bass_into_inst() {
        let mut m = ALL;
        press(&mut m, BASS, false, false);
        assert_eq!(m, ALL, "no BASS part with 3 stems");
        press(&mut m, INST, false, false);
        assert_eq!(
            gains(m, [1.0; 3], false),
            [1.0, 0.0, 0.0, 1.0],
            "bass follows INST"
        );
        let mut s = ALL;
        press(&mut s, VOCAL, true, false);
        assert_eq!(gains(s, [1.0; 3], false), [1.0, 0.0, 0.0, 0.0]);
        press(&mut s, VOCAL, true, false);
        assert_eq!(
            s, ALL,
            "solo with 3 stems ignores bass when deciding the part is alone"
        );
    }

    #[test]
    fn a_part_button_mutes_without_switching_the_strip_to_stem_iso() {
        use crate::app::{MixerState, prefs::set_for_test};
        use openfader::{
            library::{Library, own::Own},
            store::Store,
        };
        use slint::Model;
        crate::app::headless::init();
        let app = crate::app::App::new().unwrap();
        let lib: std::rc::Rc<dyn Library> =
            std::rc::Rc::new(Own::new(std::sync::Arc::new(Store::in_memory().unwrap())));
        let decks = crate::app::deck::wire(&app, lib, Default::default());
        set_for_test(|p| p.stems = true);
        crate::app::mixer::wire(&app, decks.clone());
        decks[0].show(|v| v.loaded = true);
        let mixer = app.global::<MixerState>();
        let row = || mixer.get_channels().row_data(0).unwrap();

        decks[0].stem_click(VOCAL);
        assert!(decks[0].stem_mutes()[VOCAL]);
        assert!(!row().iso, "the strip stays in EQ mode");
        assert_eq!(
            row().iso_knobs.row_data(1),
            Some(0.0),
            "the VOCAL rotary follows"
        );

        mixer.invoke_iso_toggled(0);
        decks[0].stem_click(VOCAL);
        assert!(!decks[0].stem_mutes()[VOCAL]);
        assert!(row().iso, "and a strip in STEM ISO stays there");
    }
}
