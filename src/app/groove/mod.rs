//! BEAT LAB panel ↔ ui/groove.slint (global `GrooveState`); the audio is in `src/audio/groove/`.
//!
//! Every deck has 8 DRUM SWAP SLOTs in two DRUM LOOP BANKs; the panel shows one bank at a time. A slot is filled from the browser's selected track (⤓), from a
//! Drum Preset (PRESET: the factory ones from the sampler pack set in PREFS, or the user's own; "Save as New Preset…" stores the eight
//! slots) or by DRUM CAPTURE (a range of the deck's DRUMS stem, [`capture`]); tracks decode on a worker thread ([`source`]). PLAY on a tile starts the loop on the
//! deck's next beat and stops it again; SINGLE mode replaces the deck's drums (its DRUMS stem shows off meanwhile), MULTI adds. Hold DRUM RELEASE FX (or
//! a controller's DRUM ROLL / DRUM TRANS pads) for an effect on the drums. PREFS > Extensions > DRUM SWAP: with Eject/Load Lock no load goes into a playing slot,
//! and the tiles show the loaded track's title. Controllers (controller rows of `mappings/controller.xml`, imported by `midi/rbmap`) call the same callbacks.
//!
//! A 50 ms timer takes the finished loads, pushes the tempo to the release effect and lights what the audio thread reports (slot state, playhead, whether the
//! drums are swapped). Every action that cannot work says why in the panel's status line.
//!
//! Children: [`slots`] (loading, PLAY, DRUM CAPTURE, presets: the actions on a slot), [`panel`] (release FX, the timer, PREFS), [`source`] (audio files
//! → [`Drum`](groove::Drum)), [`preset`] (Drum Preset files), [`capture`] (the DRUM CAPTURE audio and file), [`wave`] (the tile's waveform).

mod capture;
mod panel;
mod preset;
mod slots;
mod source;
mod wave;

use super::{
    App, DrumSlotView, GrooveDeckView, GrooveState,
    deck::{DECKS, Decks},
    gain, prefs,
};
use openfader::{
    audio::{
        fx::{BEATS, DEFAULT_BEAT},
        groove::{self, Controls, PER_BANK, RELEASE_FX, SLOTS, State, capture as cap},
    },
    library::Track,
};
use preset::{Entry, Preset};
use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, sync::mpsc::Receiver, time::Duration};

const WAVE_W: u32 = 120;
const WAVE_H: u32 = 24;
/// "Save as New Preset…", the last entry of the PRESET list.
const SAVE_AS: &str = n_!("Save as New Preset…");
/// DRUM ROLL / DRUM TRANS beat index of the controller's four pads (1/8, 1/4, 1/2, 1 beat) in [`BEATS`].
pub const PAD_BEATS: [usize; 4] = [1, 2, 3, 5];

/// What a slot holds besides the audio (which the audio thread owns): what its tile shows and where it came from (for presets).
struct Loop {
    title: SharedString,
    info: SharedString,
    wave: Image,
    beats: i32,
    entry: Option<Entry>,
}

/// One deck's panel state.
struct Cfg {
    bank: usize,
    /// DRUM SWAP GAIN knob 0..1 (0.5 = unity).
    knob: f32,
    loops: Vec<Option<Loop>>,
    /// DRUM RELEASE FX effect (index into [`RELEASE_FX`]) and beat (index into [`BEATS`]).
    fx: usize,
    beat: usize,
    held: Option<u8>,
    /// DRUM CAPTURE armed: the range in beats.
    cap: Option<u32>,
    /// The last range chosen with the range button.
    cap_beats: u32,
    swapped: bool,
    slots: Rc<VecModel<DrumSlotView>>,
    /// What the deck row shows now, to push it only when it changes.
    shown: Option<(usize, bool, u32, bool, usize, usize, bool, u32)>,
}

impl Cfg {
    fn new() -> Cfg {
        Cfg {
            bank: 0,
            knob: 0.5,
            loops: (0..SLOTS).map(|_| None).collect(),
            fx: 0,
            beat: DEFAULT_BEAT,
            held: None,
            cap: None,
            cap_beats: cap::RANGES[0],
            swapped: false,
            slots: Rc::new(VecModel::from(vec![DrumSlotView::default(); SLOTS])),
            shown: None,
        }
    }
}

/// A worker decoding the loops of one deck (one slot, or a whole preset): `(slot, entry, result)` per loop.
struct Pending {
    deck: usize,
    rx: Receiver<(usize, Entry, Result<groove::Drum, String>)>,
}

struct Groove {
    app: slint::Weak<App>,
    decks: Decks,
    track: Rc<RefCell<Option<Track>>>,
    model: Rc<VecModel<GrooveDeckView>>,
    cfg: RefCell<Vec<Cfg>>,
    presets: RefCell<Vec<Preset>>,
    /// The pack the factory presets were read from.
    pack: RefCell<String>,
    pending: RefCell<Vec<Pending>>,
    timer: Timer,
}

/// A slot's tile text for its gain: nothing at unity, else dB.
fn gain_text(g: f32) -> String {
    if (g - 1.0).abs() < 0.01 {
        String::new()
    } else {
        format!(" · {:+.1} dB", 20.0 * g.max(0.001).log10())
    }
}

/// The slot (0..8) a controller's DRUM SWAP pad `n` (0..4) means in bank `bank`.
pub fn pad_slot(bank: usize, n: usize) -> usize {
    bank.min(1) * PER_BANK + n.min(PER_BANK - 1)
}

/// "A1" … "B4": the name of slot `i`.
fn slot_name(i: usize) -> String {
    format!(
        "{}{}",
        if i < PER_BANK { 'A' } else { 'B' },
        i % PER_BANK + 1
    )
}

/// A slot index from a callback, kept in range.
fn slot(i: i32) -> usize {
    (i.max(0) as usize).min(SLOTS - 1)
}

impl Groove {
    fn state(&self) -> Option<App> {
        self.app.upgrade()
    }

    fn say(&self, text: impl Into<SharedString>) {
        if let Some(a) = self.state() {
            a.global::<GrooveState>().set_status(text.into());
        }
    }

    fn with<R>(&self, d: usize, f: impl FnOnce(&Controls) -> R) -> Option<R> {
        self.decks.get(d)?.with_player(|p| f(p.groove()))
    }

    /// The deck whose DRUM CAPTURE is armed and its range.
    fn armed(&self) -> Option<(usize, u32)> {
        self.cfg
            .borrow()
            .iter()
            .enumerate()
            .find_map(|(d, c)| Some((d, c.cap?)))
    }

    /// Refuses a load into a playing slot while PREFS > Extensions > DRUM SWAP is on Lock.
    fn locked(&self, d: usize, i: usize) -> bool {
        let locked = prefs::with(|p| p.extensions.drum_lock)
            && self.with(d, |c| c.state(i) != State::Idle).unwrap_or(false);
        if locked {
            self.say(tr!(
                "slot {} is playing: loading is locked (PREFS > Extensions > DRUM SWAP)",
                slot_name(i)
            ));
        }
        locked
    }

    fn edit<R>(&self, d: usize, f: impl FnOnce(&mut Cfg) -> R) -> R {
        f(&mut self.cfg.borrow_mut()[d])
    }
}

pub fn wire(app: &App, decks: &Decks, track: Rc<RefCell<Option<Track>>>) {
    let model = Rc::new(VecModel::from(vec![GrooveDeckView::default(); DECKS]));
    let st = app.global::<GrooveState>();
    st.set_decks(ModelRc::from(model.clone()));
    let me = Rc::new(Groove {
        app: app.as_weak(),
        decks: decks.clone(),
        track,
        model,
        cfg: RefCell::new((0..DECKS).map(|_| Cfg::new()).collect()),
        presets: RefCell::default(),
        pack: RefCell::default(),
        pending: RefCell::default(),
        timer: Timer::default(),
    });
    me.refresh_presets();
    me.prefs_changed();
    let weak = Rc::downgrade(&me);
    prefs::on_change(move |_| {
        if let Some(g) = weak.upgrade() {
            g.prefs_changed();
        }
    });

    on!(st, deck me, on_slot_pressed, |g, d, i| g.pressed(d, slot(i)));
    on!(st, deck me, on_slot_load, |g, d, i| g.load_track(d, slot(i)));
    on!(st, deck me, on_slot_clear, |g, d, i| g.clear(d, slot(i)));
    on!(st, deck me, on_slot_gain, |g, d, i, steps| {
        let i = slot(i);
        g.with(d, |c| {
            c.set_gain(i, (c.gain(i) + 0.1 * steps).clamp(0.0, 2.0))
        });
    });
    on!(st, deck me, on_bank_selected, |g, d, b| g
        .edit(d, |c| c.bank = b.clamp(0, 1) as usize));
    on!(st, deck me, on_multi_selected, |g, d, on| {
        g.with(d, |c| c.set_multi(on));
    });
    on!(st, deck me, on_master_edited, |g, d, v| {
        let v = v.clamp(0.0, 1.0);
        g.edit(d, |c| c.knob = v);
        g.with(d, |c| c.set_master(gain(v)));
    });
    on!(st, deck me, on_preset_chosen, |g, d, n| g
        .preset_chosen(d, n.max(0) as usize));
    on!(st, deck me, on_preset_saved, |g, d, name| g
        .preset_saved(d, name.as_str()));
    on!(st, deck me, on_fx_step, |g, d, s| g.edit(
        d,
        |c| c.fx = (c.fx as i32 + s).rem_euclid(RELEASE_FX.len() as i32) as usize
    ));
    on!(st, deck me, on_beat_step, |g, d, s| g.edit(
        d,
        |c| c.beat = (c.beat as i32 + s).clamp(0, BEATS.len() as i32 - 1) as usize
    ));
    on!(st, deck me, on_fx_held, |g, d, kind, beat, on| g.fx_held(
        d,
        kind,
        (beat.max(0) as usize).min(BEATS.len() - 1),
        on
    ));
    on!(st, deck me, on_capture_clicked, |g, d| g.capture_clicked(d));
    on!(st, deck me, on_range_clicked, |g, d| g.range_clicked(d));

    let weak = Rc::downgrade(&me);
    me.timer
        .start(TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(g) = weak.upgrade() {
                g.tick();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_pads_address_the_slot_of_the_bank_in_view() {
        assert_eq!(
            [
                pad_slot(0, 0),
                pad_slot(0, 3),
                pad_slot(1, 0),
                pad_slot(1, 3)
            ],
            [0, 3, 4, 7]
        );
        assert_eq!(pad_slot(5, 9), 7, "out of range stays in range");
        assert_eq!([0, 3, 4, 7].map(slot_name), ["A1", "A4", "B1", "B4"]);
    }

    #[test]
    fn the_roll_and_trans_pads_use_the_beat_values_of_the_effect_unit() {
        assert_eq!(PAD_BEATS.map(|i| BEATS[i]), [0.125, 0.25, 0.5, 1.0]);
    }

    #[test]
    fn slot_gain_shows_decibels_only_off_unity() {
        assert_eq!(gain_text(1.0), "");
        assert_eq!(gain_text(0.5), " · -6.0 dB");
        assert_eq!(gain_text(2.0), " · +6.0 dB");
        assert_eq!(gain(0.5), 1.0);
    }
}
