//! STEMS on a deck: the PREFS switch starts a background separation
//! ([`openfader::stems::Job`]) for each loaded library track, shown as a progress text in the stem row. The stems go to
//! the player ([`openfader::audio::Player::set_stems`]) as soon as the job starts and fill in while the track plays: frames not
//! separated yet play as the original track. SPEED separates the play position and the HOT CUEs first, QUALITY goes from the start of the track
//! ([`openfader::stems::schedule`]). The VOCAL / INST / BASS / DRUMS buttons (outlined pills: heard = outlined,
//! muted = filled; clickable as soon as a track is loaded), the STEMS pad mode, the controller's ACTIVE PART buttons
//! and the mixer's STEM-mode knobs drive
//! per-stem gains, ramped in the audio thread, so nothing clicks and key lock, scratch, loops and slip carry on.
//!
//! Gain of a stem = its mute state × its isolator knob. ACTIVE PART (PREFS: MUTE or SOLO) switches the part to zero/full
//! and moves its rotary accordingly. SOLO silences every other part (pressing the sole part again brings all back).
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

/// Button gain endpoints; INST and BASS share one rotary without muting each other.
fn activate(
    mute: &mut [bool; STEM_COUNT],
    iso: [f32; 3],
    k: usize,
    solo: bool,
    four: bool,
) -> [f32; 3] {
    *mute = gains(*mute, iso, four).map(|g| g == 0.0);
    press(mute, k, solo, four);
    let audible = [
        !mute[VOCAL],
        !mute[INST] || (four && !mute[BASS]),
        !mute[DRUMS],
    ];
    let selected = match k {
        VOCAL => 0,
        INST | BASS => 1,
        _ => 2,
    };
    std::array::from_fn(|part| {
        if solo || part == selected {
            if audible[part] { 1.0 } else { 0.0 }
        } else {
            iso[part]
        }
    })
}

pub(super) struct State {
    job: Option<Job>,
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
        let (stems, four, solo, buttons) = prefs::with(|p| {
            (
                p.stems,
                p.stems_four,
                p.active_part_solo,
                p.extensions.stem_displays()[0],
            )
        });
        let effective = gains(s.mute, s.iso, four);
        let on: Vec<bool> = effective
            .iter()
            .enumerate()
            .map(|(k, g)| *g > 0.0 && !(swapped && k == DRUMS))
            .collect();
        let (ready, text) = (s.ready, s.text.clone());
        drop(s);
        self.show(|v| {
            v.stems = stems;
            v.stem_four = four;
            v.stem_solo = solo;
            v.stem_buttons = buttons;
            v.stem_ready = ready;
            v.stem_on = ModelRc::new(VecModel::from(on));
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

    /// The separation is finished: every frame has its stems (INSTANT DOUBLES start from it).
    pub(super) fn stems_complete(&self) -> bool {
        self.stems.borrow().complete
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
        self.double.set(None); // a pending INSTANT DOUBLES belongs to the track that is going
        {
            let mut s = self.stems.borrow_mut();
            s.job = None;
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

    /// ACTIVE PART: starts separation on demand and switches the selected part to zero/full gain.
    /// SOLO retains its preference semantics; the mixer rotaries follow the resulting part levels.
    pub fn stem_press(&self, k: usize) {
        let (solo, four) = prefs::with(|p| (p.active_part_solo, p.stems_four));
        if !self.ready() || k >= STEM_COUNT || (k == BASS && !four) {
            return;
        }
        if !prefs::with(|p| p.stems) {
            prefs::update(|p| p.stems = true);
        }
        self.ensure_stems();
        let levels = {
            let mut s = self.stems.borrow_mut();
            let iso = s.iso;
            activate(&mut s.mute, iso, k, solo, four)
        };
        if let Some(app) = self.app.upgrade() {
            let mixer = app.global::<crate::app::MixerState>();
            for (part, level) in levels.into_iter().enumerate() {
                mixer.invoke_iso_knob(self.index as i32, part as i32, level);
            }
        }
        self.refresh_stem_buttons();
    }

    fn refresh_stem_buttons(&self) {
        self.apply_stems();
        self.show_stems();
        self.refresh_pads();
    }

    /// The mixer's isolator knobs (VOCAL, INST, DRUMS, 0..=1; all 1 while the channel is in EQ mode).
    pub(in crate::app) fn set_isolators(&self, iso: [f32; 3]) {
        let mut s = self.stems.borrow_mut();
        if s.iso != iso {
            s.iso = iso;
            drop(s);
            self.apply_stems();
            self.show_stems();
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
}
