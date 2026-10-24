//! Up to four playback decks (`DeckState` in ui/deck.slint, one `DeckView` row per deck). One [`Deck`]
//! type, instantiated [`DECKS`] times by [`wire`]: it loads a track's analysis (waveforms show at once),
//! decodes its audio on a worker thread, and draws the waveforms 1:1 (physical pixels) from the
//! audible position. The `decks` setting (PREFS, the 2/4 chip) decides how many are [`active`]: in 2-deck
//! mode C and D are hidden, paused and out of every deck pick; the audio engine always carries four channels.
//!
//! Base: [`beatmath`] (beat-grid maths: [`grid_bpm`], [`tempo_at`], [`beat_at`], [`ms_at`], [`snap`]) and [`wire`] (creates the
//! decks, routes the `DeckState` callbacks).
//! Children: [`load`] (analysis + async decode), [`transport`] (play/seek/tempo range/key lock/zoom/
//! beat jump/scratch), [`cue`] (player CUE state machine), [`sync`] (latched SYNC + MASTER: tempo and
//! beat following), [`pads`] (performance pads: hot cues, beat jump, keyboard, beat
//! loop, PAD FX, KEY SHIFT, SAMPLE SCRATCH), [`cuelist`] (the cue / loop list, SAVE loop, memory-cue call),
//! [`loops`] (auto, manual and halve/double loops), [`key`] (key shift, effective key, KEY SYNC), [`slip`]
//! (slip, reverse, brake), [`grid`] (GRID mode: session beat-grid edits and tap tempo), [`stems`] (STEMS:
//! separation job, per-stem gains, ACTIVE PART mute/solo), [`stemdouble`] (STEM INSTANT DOUBLES from the other
//! deck), [`tick`] (the 16 ms redraw), [`frame`] (what moves every frame: the detail strip, platter, phase meter), [`quantize`] (Q: SNAP, quantized hot cue / loop calls, REVERSE, Q on all
//! decks), [`autocue`] (the cue point a LOAD puts the playhead on), [`stemwave`] (the waves of the mix heard
//! instead of the original), [`bank`] (a Cue Bank on the HOT CUE pads), [`mixpoint`] (MIX POINT LINK:
//! the MIX OUT / MIX IN points of two decks, the panel, the automatic start of the MIX IN deck), [`groove`]
//! (what BEAT LAB (`app/groove`) reads of a deck: the DRUM CAPTURE range, the tempo, whether a DRUM SWAP
//! loop has the drums), [`exportedit`] (what EXPORT mode's GRID panel and cue panels (`app/exportmode`) add:
//! grid range, widen / narrow, BPM entry, UNDO / REDO, Analysis Lock).
//!
//! Controller entry points (what `app/midi` calls on `decks[i]`, besides the `DeckState` callbacks):
//! `play_pause`, `cue_press` / `cue_release`, `jump_to_start`, `sync_toggle`, `master_toggle`,
//! `tempo(0..1)`, `cycle_range`, `toggle_key_lock`, `scratch_begin` / `scratch_move` / `scratch_end`,
//! `bend`, `set_pad_mode`, `pad` / `pad_released` / `pad_delete`, `loop_in` / `loop_out` / `reloop` /
//! `beat_loop` / `loop_halve` / `loop_double`, `beat_jump`, `memory_cue`, `cue_call`, `toggle_delete`.

mod autocue;
pub(in crate::app) use autocue::first_sound;
mod bank;
mod beatmath;
pub(in crate::app) use beatmath::{beat_at, grid_bpm, ms_at, tempo_at};
use beatmath::{beats_between, is_loop_len, snap};
mod cue;
mod cuelist;
mod exportedit;
mod frame;
mod gain;
pub(in crate::app) mod grid;
mod groove;
mod key;
mod light;
pub(in crate::app) use light::{Pulse, lead};
mod load;
#[cfg(test)]
mod readout_tests;
pub use load::set_next_track;
mod loops;
mod mixpoint;
mod padopts;
mod pads;
pub use pads::PadMode;
mod phrase;
pub(in crate::app) use phrase::Hit;
mod quantize;
mod reedit;
mod sequencer;
mod slicer;
mod slip;
mod stemdouble;
mod stems;
mod stemwave;
mod sync;
mod tick;
mod transport;
mod wire;
pub use wire::wire;

use super::{App, DeckScroll, DeckState, DeckView, prefs, render};
use openfader::{
    analysis::{self, Beat},
    audio,
    library::{Library, Track},
};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::Receiver,
    time::Duration,
};

type Size = (usize, usize);
/// Decks the audio engine, the mixer and the model carry: A to D.
pub const DECKS: usize = audio::CHANNELS;
/// Deck letters by deck index.
pub const DECK_NAMES: [&str; DECKS] = ["A", "B", "C", "D"];
pub type Decks = [Rc<Deck>; DECKS];

/// Decks in use: the `decks` setting (2 or 4), at most [`DECKS`].
pub fn active() -> usize {
    prefs::with(|p| p.decks.clamp(2, DECKS))
}

/// The worker stays cancellable after playback starts, until optional analysis finishes.
struct Pending {
    rx: Receiver<Result<load::Decoded, String>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    title: String,
    notes: Vec<String>,
    playable: bool,
    initial_cue: u32,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Loaded {
    ready: bool,
    pending: Option<Pending>,
    /// Analysis as loaded plus this session's cue edits (session only, never written back).
    analysis: analysis::Analysis,
    /// The cues are automatic ones made for this session (the track has none saved); the first cue edit saves them.
    auto_cues: Cell<bool>,
    duration_ms: u32,
    /// The track as the browser gave it, its library (or analysed) grid for GRID RESET and its tempo.
    track: Option<Track>,
    lib_beats: Vec<Beat>,
    /// The grid before the user's stored edits, when the load knew it (our own analysis); else the library's is asked again.
    base_beats: Option<Vec<Beat>>,
    lib_bpm: f64,
    /// Tempo at rate 1.0 in BPM (grid, else library; 0 = unknown).
    base_bpm: f64,
    /// Library key (Camelot or musical name), else the detected one; empty = unknown.
    key: String,
    cue_ms: u32,
    /// The cue point is the nearest cue of LOAD SETTING: AUTO CUE leaves it.
    cue_locked: bool,
    /// AUTO GAIN in dB ([`gain`]): what the analysis measured (0 = unknown) and the user's offset on it.
    gain_analysed: f32,
    gain_adjust: f32,
    /// Active loop in track ms; the loop-in point waiting for LOOP OUT; the last loop (RELOOP).
    loop_: Option<(u32, u32)>,
    loop_in: Option<u32>,
    last_loop: Option<(u32, u32)>,
    detail: Option<render::Wave>,
    overview: Option<render::Wave>,
    /// Overview without playhead, valid for the size it was rendered at.
    overview_base: Option<render::Canvas>,
    /// The detail picture the view slides over ([`frame`]); `None` = draw it again.
    strip: Option<frame::Strip>,
    overview_key: Option<(i64, Size, Option<i64>, u32, bool)>,
    time_key: Option<u32>,
    /// GRID panel undo / redo lists and range ([`exportedit`]); gone with the next LOAD.
    grid_hist: exportedit::Hist,
}

impl Loaded {
    /// Forget what was drawn: cues, duration or waveform changed.
    fn invalidate(&mut self) {
        self.strip = None;
        self.overview_key = None;
        self.overview_base = None;
        self.time_key = None;
    }
}

pub struct Deck {
    index: usize,
    app: slint::Weak<App>,
    model: Rc<VecModel<DeckView>>,
    /// `DeckState.scroll`: what moves every frame ([`frame`]).
    scroll: Rc<VecModel<DeckScroll>>,
    /// Mirror of this deck's `DeckView` row; pushed to the model by [`Deck::show`] / `tick`.
    view: RefCell<DeckView>,
    player: Option<RefCell<audio::Player>>,
    loaded: RefCell<Loaded>,
    /// Playback speed, 1.0 = normal (the player's own rate is kept equal).
    rate: Cell<f64>,
    /// Index into [`transport::RANGES`].
    range: Cell<usize>,
    /// Detail window width in ms.
    span_ms: Cell<u32>,
    /// Cue / pad / loop-in snap to the nearest beat.
    quantize: Cell<bool>,
    /// Manual key shift in whole semitones (the player's own shift is kept equal).
    key_st: Cell<i32>,
    /// PAD FX pad currently held (its effect is on), see [`pads`].
    pad_fx: Cell<Option<usize>>,
    /// Page of the current pad mode (see `pads::page_range`); 0 after a mode change.
    pad_page: Cell<i32>,
    /// Logical size of the two waveform elements, reported by the layout.
    detail_size: Cell<(f32, f32)>,
    overview_size: Cell<(f32, f32)>,
    /// Tap-tempo series of GRID mode.
    tap: RefCell<grid::Tap>,
    /// Counts LOADs (see [`Deck::track`]).
    load_serial: Cell<u32>,
    /// SYNC / MASTER state ([`sync`]) and the CUE button's preview state ([`cue`]).
    sync: sync::State,
    cue_state: Cell<cue::Preview>,
    /// STEMS: the separation job, the mute state and the isolator knobs ([`stems`]).
    stems: RefCell<stems::State>,
    /// The STEM waveform: copies of the audio and stems, the parked original waves ([`stemwave`]).
    stemwave: RefCell<stemwave::State>,
    /// Part of a pending STEM INSTANT DOUBLES: waits for this deck's stems ([`stemdouble`]).
    double: RefCell<Option<stemdouble::Pending>>,
    /// Quantize NEXT BEAT: the hot cue / loop waiting for its boundary (ms) and the playhead at the last tick ([`quantize`]).
    pending: Cell<Option<(f64, f64, quantize::Call)>>,
    /// HOT CUE gate: the cue a held pad returns to when released ([`pads`]).
    gate: Cell<Option<u32>>,
    /// The plugin pad mode and pad held now, whose `release` steps run when it lets go ([`pads`]).
    plugin_held: Cell<Option<(&'static openfader::plugin::PadModeDecl, usize)>>,
    /// JOG CUE RETURN: the platter was touched while paused on the cue point ([`transport`]).
    jog_cue: Cell<bool>,
    /// QUANTIZE REVERSE: REVERSE engaged slip itself, REVERSE off disengages it ([`slip`]).
    rev_slip: Cell<bool>,
    /// Play as soon as the decode lands (CONTINUOUS PLAYING, [`load`]).
    autoplay: Cell<bool>,
    /// The plugin-source track being downloaded for this deck ([`load`]).
    download: RefCell<Option<load::Download>>,
    /// The library `load` takes a track's analysis from.
    lib: Rc<dyn Library>,
    /// The Cue Bank on the HOT CUE pads, if one was loaded ([`bank`]).
    bank: RefCell<Option<bank::Armed>>,
    /// SLICER / SLICE LOOP range, roll and the held pad ([`slicer`]); the SAMPLER pad held ([`sequencer`]).
    slicer: slicer::State,
    sequencer: sequencer::State,
    _timer: Timer,
}

pub(in crate::app) fn fmt_ms(ms: u32) -> String {
    format!("{}:{:02}.{}", ms / 60_000, ms / 1000 % 60, ms / 100 % 10)
}

fn bpm_text(grid_bpm: Option<f64>, rate: f64) -> slint::SharedString {
    let digits = prefs::with(|p| p.view.tempo_decimals.digits());
    grid_bpm.map_or("— BPM".into(), |g| {
        format!("{:.*} BPM", digits, g * rate).into()
    })
}

fn tempo_text(rate: f64) -> slint::SharedString {
    format!("{:+.1}%", (rate - 1.0) * 100.0).into()
}

/// Whether `pos_ms` sits at the end of a track of `duration_ms` (the player clamps there when it stops).
fn at_end(pos_ms: f64, duration_ms: u32) -> bool {
    duration_ms > 0 && pos_ms + 50.0 >= duration_ms as f64
}

/// Last 30 s of a playing track: the overview warns (blinks red).
fn ending(pos_ms: f64, duration_ms: u32, playing: bool) -> bool {
    playing && duration_ms > 0 && duration_ms as f64 - pos_ms <= 30_000.0
}

/// Hot-cue pad letter A to H (`pad` 0-based).
fn pad_label(pad: usize) -> char {
    (b'A' + pad as u8) as char
}

impl Deck {
    fn new(
        app: &App,
        index: usize,
        model: Rc<VecModel<DeckView>>,
        scroll: Rc<VecModel<DeckScroll>>,
        link: Rc<sync::Link>,
        lib: Rc<dyn Library>,
    ) -> Rc<Self> {
        let (player, status) = match audio::Player::new() {
            Ok(p) => (
                Some(RefCell::new(p)),
                tr!("Select a track, then LOAD").to_owned(),
            ),
            Err(e) => (None, tr!("audio disabled: {}", e)),
        };
        // Start-up defaults from the settings.
        let (range, key_lock_default, quantize, auto_beats) = prefs::with(|p| {
            (
                p.tempo_range_default.min(transport::RANGES.len() - 1),
                p.key_lock_default,
                p.quantize_default,
                p.controller.loop_beats() as i32,
            )
        });
        let key_lock = player.as_ref().is_some_and(|pl| {
            pl.borrow_mut().set_key_lock(key_lock_default);
            key_lock_default
        });
        let span_ms = render::detail::DEFAULT_SPAN_MS;
        let view = DeckView {
            status: status.into(),
            tempo: 0.5,
            bpm_text: bpm_text(None, 1.0),
            tempo_text: tempo_text(1.0),
            tempo_range: crate::app::i18n::tr_dyn(transport::RANGES[range].1).into(),
            zoom_text: transport::span_text(span_ms).into(),
            quantize,
            auto_beats,
            key_lock,
            pads: ModelRc::new(VecModel::from(pads::views(&analysis::Analysis::default()))),
            ..Default::default()
        };
        model.set_row_data(index, view.clone());
        let deck = Rc::new(Self {
            index,
            app: app.as_weak(),
            model,
            scroll,
            view: RefCell::new(view),
            player,
            loaded: RefCell::default(),
            rate: Cell::new(1.0),
            range: Cell::new(range),
            span_ms: Cell::new(span_ms),
            quantize: Cell::new(quantize),
            key_st: Cell::new(0),
            pad_page: Cell::new(0),
            pad_fx: Cell::new(None),
            detail_size: Cell::default(),
            overview_size: Cell::default(),
            tap: RefCell::default(),
            load_serial: Cell::new(0),
            sync: prefs::with(|p| sync::State::new(link, &p.controller)),
            cue_state: Cell::default(),
            stems: RefCell::default(),
            stemwave: RefCell::default(),
            double: RefCell::new(None),
            pending: Cell::new(None),
            gate: Cell::new(None),
            plugin_held: Cell::new(None),
            jog_cue: Cell::new(false),
            rev_slip: Cell::new(false),
            autoplay: Cell::new(false),
            download: RefCell::new(None),
            lib,
            bank: RefCell::new(None),
            slicer: slicer::State::default(),
            sequencer: sequencer::State::default(),
            _timer: Timer::default(),
        });
        deck.show_stems();
        deck.apply_vinyl();
        // Display rate. tick() is cheap when nothing changed: it redraws only while the audible
        // position moves (playing) or the buffer size changes. Weak: the timer must not keep the deck alive.
        let d = Rc::downgrade(&deck);
        deck._timer
            .start(TimerMode::Repeated, Duration::from_millis(16), move || {
                if let Some(d) = d.upgrade() {
                    d.tick();
                }
            });
        deck
    }

    /// Edits this deck's view and pushes it to Slint; an edit that changes nothing pushes nothing, so no
    /// bindings re-evaluate.
    fn show(&self, f: impl FnOnce(&mut DeckView)) {
        let snapshot = {
            let mut v = self.view.borrow_mut();
            let before = v.clone();
            f(&mut v);
            if *v == before {
                return;
            }
            v.clone()
        };
        self.model.set_row_data(self.index, snapshot);
    }

    fn say(&self, text: impl Into<slint::SharedString>) {
        self.show(|v| v.status = text.into());
    }

    /// Track loaded and audio decoded: the transport, cue and loop controls work.
    pub(in crate::app) fn ready(&self) -> bool {
        self.view.borrow().loaded
    }

    /// This deck's place: 0 = A … 3 = D.
    pub(in crate::app) fn index(&self) -> usize {
        self.index
    }

    /// Is the deck's audio playing? `false` without audio.
    pub(in crate::app) fn playing(&self) -> bool {
        self.with_player(|p| p.is_playing()).unwrap_or(false)
    }

    /// Playback rate, 1.0 = normal.
    pub(in crate::app) fn rate(&self) -> f64 {
        self.rate.get()
    }

    /// Runs `f` on the loaded analysis (this session's cues and grid included) and the duration in ms.
    pub(in crate::app) fn with_analysis<R>(
        &self,
        f: impl FnOnce(&analysis::Analysis, u32) -> R,
    ) -> R {
        let l = self.loaded.borrow();
        f(&l.analysis, l.duration_ms)
    }

    /// The track last passed to `load` and how many loads this deck has had, `None` before the first.
    pub(in crate::app) fn track(&self) -> Option<(u32, Track)> {
        self.loaded
            .borrow()
            .track
            .clone()
            .map(|t| (self.load_serial.get(), t))
    }

    /// The load count [`Deck::track`] returns, without cloning the track: it identifies what is loaded.
    pub(in crate::app) fn serial(&self) -> Option<u32> {
        self.loaded
            .borrow()
            .track
            .is_some()
            .then(|| self.load_serial.get())
    }

    /// Runs `f` on the loaded track without cloning it; `None` while the deck is empty.
    pub(in crate::app) fn with_track<R>(&self, f: impl FnOnce(&Track) -> R) -> Option<R> {
        self.loaded.borrow().track.as_ref().map(f)
    }

    /// Audible position (ms) and play state; a deck without audio sits at its cue.
    fn clock(&self, l: &Loaded) -> (f64, bool) {
        self.player.as_ref().map_or((l.cue_ms as f64, false), |p| {
            let p = p.borrow();
            (p.position_ms(), p.is_playing())
        })
    }

    /// Playhead position for setting a cue or loop-in: on the nearest SNAP fraction of a beat when quantize is on.
    fn mark(&self, l: &Loaded) -> u32 {
        let (pos, _) = self.clock(l);
        if self.quantize.get() {
            snap(
                &l.analysis.beats,
                pos,
                prefs::with(|p| p.controller.snap_length.beats()),
            )
        } else {
            pos as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` beats from `start` ms, 500 ms apart (120 BPM).
    pub(super) fn grid(start: u32, n: u32) -> Vec<Beat> {
        (0..n)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: start + 500 * i,
            })
            .collect()
    }

    #[test]
    fn ending_is_the_last_30_seconds_while_playing() {
        assert!(!ending(10_000.0, 100_000, true));
        assert!(ending(70_000.0, 100_000, true));
        assert!(!ending(70_000.0, 100_000, false));
        assert!(!ending(0.0, 0, true));
        assert!(at_end(99_970.0, 100_000) && !at_end(50_000.0, 100_000) && !at_end(0.0, 0));
    }
}
