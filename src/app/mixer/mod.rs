//! Mixer ↔ ui/mixer.slint (global `MixerState`): per channel (four: decks A to D; C and D show in the 4-deck layout) TRIM,
//! HI/MID/LOW EQ, CFX (the colour-FX knob; its type is the global `MixerState.cfx-type`), a fader, a headphone CUE button, a
//! crossfader assignment (A / THRU / B) and a VU meter, plus the crossfader (SMOOTH / SHARP / CONST curve, see [`curve`]) and the
//! master / headphone section (MASTER level + post-sum VU, HP MIX, HP LEVEL, SPLIT, REC). One [`Mixer`] drives all decks' players;
//! the `MixerState.channels` / `.bus` model rows are the single source of truth for every knob.
//!
//! Applied gain = (trim + the loaded track's AUTO GAIN) × the channel fader's curve × crossfade side, sent through
//! `Player::set_volume` (clamped to 0..=4 = +12 dB; above 1.0 the device clips). The cue feed is trim (+ auto gain) only
//! (pre-fader, pre-crossfade), sent through `Player::set_cue_gain`. The bus knobs, times the Output Level setting for MASTER, go to
//! the shared `audio::Bus`, whose routing (SPLIT = cue left, master right, or a second headphone device via
//! `OPENFADER_CUE_DEVICE`) lives in `src/audio/bus.rs`. REC ([`rec`]) records the bus tap; the bus `status` line carries the
//! headphone device (or why it failed) and the last REC result.
//!
//! Preferences (CONTROLLER > Mixer / Effect, EXTENSIONS) are read through [`cfg`] and act live: EQ type (ISOLATOR kills a band at
//! the bottom), channel fader curve, output level, auto gain, the STEM button's display (PREFS: STEM ISO), and `MixerState.release-held` (set by
//! the FX panel) which, with "Apply RELEASE FX on CFX", puts the COLOR FX knobs at centre while held. The DUAL PLAYER's AUTO GAIN
//! knob (`ChannelView.auto-gain`, `MixerState.auto-gain-knob`) edits the loaded track's stored offset on its analysed gain
//! (`Deck::set_gain_adjust`); the TRIM knob stays the user's own; the sum reaches the channel only while the AUTO GAIN pref is on.
//!
//! CFX: knob 4 of each channel (`MixerState.invoke_knob(ch, 4, v)`) and the type chips (`invoke_cfx_selected(i)`, index into
//! `audio::cfx::names()`: the `Kind`s, then the COLOR plugin effects) go to `Player::cfx()`; the knob is the player's filter while the type is FILTER. The tempo of the
//! tempo-synced types is the channel's own deck's effective BPM, pushed on every tick. Automix drives the crossfader position
//! itself and sets `MixerState.auto-fade` (constant-power gains whatever the curve); its styles move the strips' fader, EQ and COLOR FX
//! knobs through the channel rows (`automix::rig`), so the strips show them moving, and put the DJ's own values back afterwards.
//!
//! AUX: the strip after the decks (`MixerState.channels[`[`AUX_CH`]`]`, shown while PREFS > AUDIO > "Enable AUX input" is on) has the
//! deck strip's TRIM, HIGH / MID / LOW, FILTER, LEVEL fader, CUE and meter, plus ON ([`auxin`]); it drives [`openfader::audio::auxin`].
//!
//! Children: [`channel`] (one strip: knobs, EQ / STEM mode, CFX, CUE, assignment), [`auxin`] (the AUX strip), [`bus`] (master / headphone knobs), [`map`] (knob
//! position ↔ dB / CFX value), [`vu`] (meter ballistics), [`curve`] (crossfader and channel fader curves), [`cfg`], [`rec`] (the REC
//! button), [`sheet`] and [`control`] (the cue sheet, tracklist and control log REC writes next to a recording).

mod auxin;
mod bus;
mod cfg;
mod channel;
mod control;
mod curve;
mod map;
mod rec;
mod sheet;
mod vu;
pub(in crate::app) use channel::{iso_at, knob_at};
pub(in crate::app) use control::note;
pub(in crate::app) use map::{CFX, HI, LOW, MID};

use super::{
    App, BusView, ChannelView, DeckState, MixerState,
    deck::{DECKS, Decks},
    prefs::{self, CrossfaderCurve},
};
use cfg::{Cfg, auto_db};
use openfader::audio::{
    Bus, Headphones,
    cfx::{self, Kind},
};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// The AUX strip's row in `MixerState.channels`, after the decks.
pub(in crate::app) const AUX_CH: usize = DECKS;

/// Linear cue-feed gain of one channel: trim while CUE is on (pre-fader, pre-crossfade), else silent.
fn cue_gain(trim_db: f32, on: bool) -> f32 {
    if on { map::db_to_gain(trim_db) } else { 0.0 }
}

/// Linear player gain for one channel.
fn gain(trim_db: f32, fader: f32, side: f32) -> f32 {
    map::db_to_gain(trim_db) * fader * side
}

/// Disabled manual control leaves both sides open; Automix still applies its transition gains.
fn crossfade_gains(s: &MixerState) -> [f32; 2] {
    let curve = if s.get_auto_fade() {
        CrossfaderCurve::Constant
    } else if !s.get_crossfader_enabled() {
        return [1.0, 1.0];
    } else {
        curve::from_index(s.get_curve())
    };
    curve::sides(curve, s.get_crossfader())
}

/// Each channel's crossfade gain from the current `MixerState` (a channel's A / THRU / B assignment picks its side, THRU is 1).
/// The one place that turns the crossfader into gains for all channels: the mixer and Session history (what is on air) both call it.
pub(in crate::app) fn sides(app: &App) -> [f32; DECKS] {
    let s = app.global::<MixerState>();
    let ab = crossfade_gains(&s);
    let channels = s.get_channels();
    std::array::from_fn(|i| side_gain(channels.row_data(i).map_or(0, |c| c.xf), ab))
}

/// A channel's crossfade gain: `xf` 0 = its A side, 2 = its B side, 1 = THRU (not on the crossfader).
fn side_gain(xf: i32, [a, b]: [f32; 2]) -> f32 {
    match xf {
        0 => a,
        2 => b,
        _ => 1.0,
    }
}

struct Mixer {
    app: slint::Weak<App>,
    decks: Decks,
    /// The stream's shared master/headphone knobs; None when no deck has audio.
    bus: Option<Arc<Bus>>,
    model: Rc<VecModel<ChannelView>>,
    vus: RefCell<[vu::Vu; DECKS]>,
    master_vu: RefCell<vu::Vu>,
    /// The (bar, peak) heights last written to each channel row / the bus, so a still meter costs no model write.
    shown_levels: [Cell<(f32, f32)>; DECKS],
    shown_master: Cell<(f32, f32)>,
    /// The AUX strip's meter, the last (bar, peak) written to its row, and the problem its input last reported.
    aux_vu: RefCell<vu::Vu>,
    shown_aux: Cell<(f32, f32)>,
    aux_err: RefCell<Option<String>>,
    last_tick: Cell<Instant>,
    cfg: Cell<Cfg>,
    /// The RELEASE-held state `apply` last used, and the auto-gain dB of each channel (the tick re-applies on a change).
    release: Cell<bool>,
    auto: [Cell<f32>; DECKS],
    /// The deck's AUTO GAIN (effective dB, adjustment) the channel row shows.
    shown: [Cell<(f32, f32)>; DECKS],
    /// The running recording, and the recorded files already handed to the library.
    rec: RefCell<Option<rec::Rec>>,
    imported: RefCell<Vec<PathBuf>>,
    timer: Timer,
}

pub fn wire(app: &App, decks: Decks) {
    let (cfg, curve, crossfader_enabled) = prefs::with(|p| {
        (
            Cfg::of(p),
            p.crossfader_curve,
            p.controller.crossfader_enabled,
        )
    });
    // Each row owns its inner models: cloning one row would share them between the channels. Decks A and C
    // start on the crossfader's A side, B and D on its B side.
    let channel = |i: usize| ChannelView {
        knobs: ModelRc::new(VecModel::from(vec![map::CENTRE; map::KNOBS])),
        readouts: ModelRc::new(VecModel::from(
            (0..map::KNOBS)
                .map(|k| map::readout(Kind::Filter, cfg.eq, k, map::CENTRE).into())
                .collect::<Vec<_>>(),
        )),
        fader: 0.2, // channels start low: push a fader up to hear the deck
        xf: if i % 2 == 0 { 0 } else { 2 },
        iso_knobs: ModelRc::new(VecModel::from(vec![1.0; 3])),
        auto_gain: map::CENTRE,
        auto_gain_text: map::auto_readout(0.0).into(),
        ..Default::default()
    };
    let model = Rc::new(VecModel::from((0..=DECKS).map(channel).collect::<Vec<_>>()));
    let state = app.global::<MixerState>();
    state.set_channels(ModelRc::from(model.clone()));
    state.set_cfx_names(ModelRc::new(VecModel::from(
        cfx::names()
            .into_iter()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    )));
    let bus = decks.iter().find_map(|d| d.with_player(|p| p.bus()));
    state.set_bus(BusView {
        knobs: ModelRc::new(VecModel::from(map::BUS_START.to_vec())),
        readouts: ModelRc::new(VecModel::from(
            map::BUS_START
                .iter()
                .enumerate()
                .map(|(i, &k)| map::bus_readout(i, k).into())
                .collect::<Vec<_>>(),
        )),
        status: match bus.as_deref().map(Bus::headphones) {
            Some(Headphones::Device(name)) => tr!("Headphones: {}", name),
            Some(Headphones::Failed(why)) => {
                eprintln!("mixer: {why}");
                why.clone()
            }
            _ => String::new(),
        }
        .into(),
        ..Default::default()
    });
    let mixer = Rc::new(Mixer {
        app: app.as_weak(),
        decks,
        bus,
        model,
        vus: RefCell::new(std::array::from_fn(|_| vu::Vu::new())),
        master_vu: RefCell::new(vu::Vu::new()),
        shown_levels: std::array::from_fn(|_| Cell::new((0.0, 0.0))),
        shown_master: Cell::new((0.0, 0.0)),
        aux_vu: RefCell::new(vu::Vu::new()),
        shown_aux: Cell::new((0.0, 0.0)),
        aux_err: RefCell::new(None),
        last_tick: Cell::new(Instant::now()),
        cfg: Cell::new(cfg),
        release: Cell::new(false),
        auto: std::array::from_fn(|_| Cell::new(0.0)),
        shown: std::array::from_fn(|_| Cell::new((0.0, 0.0))),
        rec: RefCell::new(None),
        imported: RefCell::new(Vec::new()),
        timer: Timer::default(),
    });
    state.set_curve(curve::index(curve));
    state.set_crossfader_enabled(crossfader_enabled);
    state.set_iso_shown(cfg.iso_shown);
    state.set_cfx_stop(cfg.cfx_stop());
    mixer.apply_all();
    prefs::with(|p| mixer.sync_aux(p));

    let m = mixer.clone();
    state.on_knob(move |ch, knob, v| m.set_knob(ch as usize, knob as usize, v));
    let m = mixer.clone();
    state.on_auto_gain_knob(move |ch, v| m.set_auto_gain(ch as usize, v));
    let m = mixer.clone();
    state.on_cfx_selected(move |i| m.select_cfx(i));
    let m = mixer.clone();
    state.on_fader(move |ch, v| {
        m.update(ch as usize, |row| row.fader = v.clamp(0.0, 1.0));
        m.apply(ch as usize);
    });
    let m = mixer.clone();
    state.on_crossfade(move |_| m.apply_all());
    // The CURVE chip and the PREFS dialog edit the same setting: prefs is the truth, the property mirrors it.
    state.on_curve_cycled(|| {
        prefs::update(|p| p.crossfader_curve = curve::next(p.crossfader_curve))
    });
    let m = mixer.clone();
    prefs::on_change(move |p| {
        let Some(app) = m.app.upgrade() else { return };
        let cfg = Cfg::of(p);
        let old = m.cfg.replace(cfg);
        let s = app.global::<MixerState>();
        s.set_curve(curve::index(p.crossfader_curve));
        s.set_crossfader_enabled(p.controller.crossfader_enabled);
        s.set_iso_shown(cfg.iso_shown);
        s.set_cfx_stop(cfg.cfx_stop());
        if !cfg.stems || !cfg.iso_shown {
            (0..DECKS)
                .filter(|&ch| m.row(ch).iso)
                .for_each(|ch| m.toggle_iso(ch)); // no isolators without STEMS or their display: back to the EQ
        }
        if old.eq != cfg.eq {
            (0..DECKS).for_each(|ch| m.show_eq(ch));
        }
        m.apply_all();
        m.sync_aux(p);
    });
    let m = mixer.clone();
    state.on_cue_toggled(move |ch| m.toggle_cue(ch as usize));
    let m = mixer.clone();
    state.on_aux_toggled(move || m.toggle_aux());
    let m = mixer.clone();
    state.on_xf_assigned(move |ch, side| m.assign_xf(ch as usize, side));
    let m = mixer.clone();
    state.on_iso_toggled(move |ch| m.toggle_iso(ch as usize));
    let m = mixer.clone();
    state.on_iso_knob(move |ch, part, v| m.set_iso_knob(ch as usize, part as usize, v));
    let m = mixer.clone();
    state.on_iso_part(move |ch, part, v| m.set_part_level(ch as usize, part as usize, v));
    let m = mixer.clone();
    state.on_bus_knob(move |knob, v| m.set_bus_knob(knob as usize, v));
    let m = mixer.clone();
    state.on_split_toggled(move || m.toggle_split());
    let m = mixer.clone();
    state.on_rec_toggled(move || m.toggle_rec());

    let weak = Rc::downgrade(&mixer);
    mixer
        .timer
        .start(TimerMode::Repeated, Duration::from_millis(33), move || {
            if let Some(m) = weak.upgrade() {
                m.tick();
            }
        });
}

impl Mixer {
    /// The model row of channel `ch`.
    fn row(&self, ch: usize) -> ChannelView {
        self.model.row_data(ch).expect("a mixer channel")
    }

    fn update(&self, ch: usize, f: impl FnOnce(&mut ChannelView)) {
        let mut row = self.row(ch);
        f(&mut row);
        self.model.set_row_data(ch, row);
    }

    fn apply_all(&self) {
        (0..DECKS).for_each(|ch| self.apply(ch));
        self.apply_bus();
    }

    fn edit_bus(&self, f: impl FnOnce(&mut BusView)) {
        if let Some(app) = self.app.upgrade() {
            let state = app.global::<MixerState>();
            let mut bus = state.get_bus();
            f(&mut bus);
            state.set_bus(bus);
        }
    }

    fn set_status(&self, text: String) {
        eprintln!("mixer: {text}");
        self.edit_bus(|b| b.status = text.into());
    }

    fn tick(&self) {
        self.tick_rec();
        let app = self.app.upgrade();
        // RELEASE FX pressed / released, or another track's auto gain after a load: push the channel again.
        let released = app
            .as_ref()
            .is_some_and(|a| a.global::<MixerState>().get_release_held());
        let release_changed = released != self.release.get();
        let auto_on = self.cfg.get().auto_gain;
        for ch in 0..DECKS {
            let auto = auto_db(auto_on, self.show_auto(ch));
            if release_changed || self.auto[ch].get() != auto {
                self.apply(ch);
            }
        }
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_tick.replace(now))
            .as_secs_f32();
        let deck_views = app.as_ref().map(|a| a.global::<DeckState>().get_decks());
        let mut vus = self.vus.borrow_mut();
        for (ch, vu) in vus.iter_mut().enumerate() {
            let bpm = deck_views
                .as_ref()
                .and_then(|m| m.row_data(ch))
                .map_or(0.0, |d| d.bpm);
            let level = self.decks[ch]
                .with_player(|p| {
                    p.cfx().set_bpm(bpm);
                    p.level()
                })
                .unwrap_or(0.0);
            vu.update(level, dt);
            let shown = (vu.level(), vu.peak());
            if self.shown_levels[ch].replace(shown) != shown {
                self.update(ch, |row| (row.level, row.peak) = shown);
            }
        }
        let mut master = self.master_vu.borrow_mut();
        master.update(self.bus.as_ref().map_or(0.0, |b| b.master_level()), dt);
        let shown = (master.level(), master.peak());
        if self.shown_master.replace(shown) != shown {
            self.edit_bus(|b| (b.level, b.peak) = shown);
        }
        self.tick_aux(dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::audio::equal_power as crossfade;

    #[test]
    fn gain_is_trim_times_fader_times_crossfade() {
        let [a, b] = crossfade(0.5);
        // defaults: trim 0 dB, fader open → only the crossfade (−3 dB) remains
        assert!(
            (gain(0.0, 1.0, a) - 0.707).abs() < 1e-3
                && (gain(0.0, 1.0, a) - gain(0.0, 1.0, b)).abs() < 1e-6
        );
        assert_eq!(gain(f32::NEG_INFINITY, 1.0, 1.0), 0.0); // trim fully down
        assert_eq!(gain(0.0, 0.0, 1.0), 0.0); // fader closed
        assert!(gain(0.0, 1.0, crossfade(1.0)[0]) < 1e-6); // A cut at the far end
        // +12 dB trim on a half-open fader is a 2× boost (3.98 × 0.5)
        assert!((gain(12.0, 0.5, 1.0) - 1.9905).abs() < 1e-3);
        // staged through the knob mapping: centre trim is unity
        assert_eq!(gain(map::trim_db(map::CENTRE), 1.0, 1.0), 1.0);
    }

    #[test]
    fn crossfader_assignment_picks_a_side_or_thru() {
        let ab = crossfade(0.25);
        assert_eq!(
            [0, 1, 2, 9].map(|xf| side_gain(xf, ab)),
            [ab[0], 1.0, ab[1], 1.0]
        );
        // crossfader fully on A: a channel assigned to B is cut, one on A and a THRU channel play at full gain
        let ab = crossfade(0.0);
        assert_eq!(
            (side_gain(0, ab), side_gain(1, ab), side_gain(2, ab)),
            (1.0, 1.0, 0.0)
        );
    }

    #[test]
    fn cue_feed_is_trim_only_and_follows_the_button() {
        assert_eq!(cue_gain(0.0, true), 1.0); // fader and crossfader do not enter
        assert_eq!(cue_gain(0.0, false), 0.0);
        assert!((cue_gain(-6.0, true) - 0.501).abs() < 1e-3);
        assert_eq!(cue_gain(f32::NEG_INFINITY, true), 0.0); // trim fully down silences the cue too
    }

    #[test]
    fn auto_gain_adds_to_the_trim() {
        // +6 dB auto on a centred trim doubles the gain; a closed trim stays closed
        assert!(
            (gain(map::trim_db(map::CENTRE) + auto_db(true, 6.0), 1.0, 1.0) - 1.995).abs() < 1e-3
        );
        assert_eq!(gain(map::trim_db(0.0) + auto_db(true, 6.0), 1.0, 1.0), 0.0);
    }

    #[test]
    fn stored_gain_and_adjustment_reach_the_channel_only_with_the_pref() {
        // what a deck gets on LOAD: the store's analysed + adjust (-4 + 6 = +2 dB), what `Mixer::apply` adds to the trim
        let st = openfader::store::Store::in_memory().unwrap();
        st.set_gain_analysed("a|1", -4.0).unwrap();
        st.set_gain_adjust("a|1", 6.0).unwrap();
        let deck_db = st.gain("a|1").unwrap().unwrap().effective_db();
        let trim = map::trim_db(map::CENTRE);
        let on = gain(trim + auto_db(true, deck_db), 1.0, 1.0);
        assert!((on - map::db_to_gain(2.0)).abs() < 1e-6, "{on}");
        assert_eq!(gain(trim + auto_db(false, deck_db), 1.0, 1.0), 1.0); // pref off: the trim alone
        // the adjustment never takes the sum past +-12 dB
        st.set_gain_analysed("a|1", 10.0).unwrap();
        let capped = st.gain("a|1").unwrap().unwrap().effective_db();
        assert_eq!(auto_db(true, capped), 12.0);
    }
}
