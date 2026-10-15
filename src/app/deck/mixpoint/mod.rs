//! MIX POINT LINK, the panel of ui/mixpoint.slint (global
//! `MixPointState`): MIX OUT and MIX IN points on two decks, a link between them, and the MIX IN deck starting by itself when
//! the MIX OUT deck gets to its point, in step with it.
//!
//! The link: the MIX IN deck is started from its own beginning ([`plan::start_at`]) so that both points reach the speakers
//! together; with SILENT it is muted from the beginning of its track to the silent end. A running IN deck that drifted is put
//! back in step by pressing LINK again. A deck that gets a new track loses its points and links.
//!
//! Base: the timer, the prefs, the links and SILENT. Children: [`input`] (the user's clicks), [`push`] (the Slint side),
//! [`session`] (what is selected, set and linked; pure), [`plan`] (the maths; pure), [`view`] (the panel's pictures).

mod input;
mod plan;
mod push;
mod session;
mod view;

use super::{DECKS, Deck, Decks, Loaded, fmt_ms, pad_label, prefs, snap};
use crate::app::prefs::controller::{Controller, MixPointDefault};
use crate::app::{App, MixDeckView, MixPointState, toast};
use plan::{Lane, View};
use session::{Mode, Session};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

/// The link logic and the panel are redone this often.
const TICK: Duration = Duration::from_millis(20);
/// A click this close to a cue marker (fraction of the enlarged waveform's width) takes the cue's own time.
const CUE_HIT: f64 = 0.012;
/// Numbers of the lane / overview sizes reported by the panel.
const LANE_OUT: usize = 0;
const LANE_IN: usize = 1;
const OVERVIEW: usize = 2;
/// Fields of the redraw key (see [`Mix::render`]).
const KEY: usize = 25;

/// PREFS > Controller > MIX POINT LINK, as the tick reads it (a copy: reading the settings clones them all).
#[derive(Clone, Copy)]
struct Opts {
    enabled: bool,
    auto_switch: bool,
    auto_sync: bool,
    zoom_limit: bool,
    default: Mode,
    view: view::Opts,
}

impl Opts {
    fn of(c: &Controller) -> Self {
        Self {
            enabled: c.mix_point_link,
            auto_switch: c.mix_point_auto_switch,
            auto_sync: c.mix_point_auto_sync,
            zoom_limit: c.mix_point_zoom_limit,
            default: match c.mix_point_default {
                MixPointDefault::In => Mode::In,
                MixPointDefault::Out => Mode::Out,
            },
            view: view::Opts {
                time: c.mix_point_time,
                grids: c.mix_point_grids,
                grids_enlarged_only: c.mix_point_grids_enlarged,
            },
        }
    }
}

/// What the panel needs of a deck at one moment.
struct Probe {
    ready: bool,
    playing: bool,
    /// Audible position, track ms.
    pos: f64,
    rate: f64,
    duration: f64,
    cue_ms: u32,
    /// The enlarged waveform's window, ms.
    span: f64,
    quantize: bool,
    serial: u32,
}

fn probe(d: &Deck) -> Probe {
    let l = d.loaded.borrow();
    let (pos, playing) = d.clock(&l);
    Probe {
        ready: d.ready(),
        playing,
        pos,
        rate: d.rate().max(0.1),
        duration: f64::from(l.duration_ms),
        cue_ms: l.cue_ms,
        span: f64::from(d.span_ms.get()),
        quantize: d.quantize.get(),
        serial: d.load_serial.get(),
    }
}

/// Cue times of a loaded deck, track ms.
fn cue_times(l: &Loaded) -> impl Iterator<Item = u32> + Clone + '_ {
    l.analysis.cues.iter().map(|c| c.time_ms)
}

struct Mix {
    s: RefCell<Session>,
    decks: Decks,
    app: slint::Weak<App>,
    opts: Cell<Opts>,
    /// The panel's window on the time axis and the axis range it can show.
    view: Cell<View>,
    ext: Cell<(f64, f64)>,
    /// Logical sizes of the MIX OUT lane, the MIX IN lane and the overview.
    sizes: Cell<[(f32, f32); 3]>,
    /// The deck's load count last seen (a new load clears the deck's points), who is muted by SILENT, whose BEAT SYNC was
    /// turned into BPM SYNC for the mix.
    serial: [Cell<u32>; DECKS],
    cutting: [Cell<bool>; DECKS],
    forced: [Cell<bool>; DECKS],
    /// What the pictures were last drawn from.
    drawn: Cell<[u64; KEY]>,
    model: Rc<VecModel<MixDeckView>>,
    timer: Timer,
}

/// Connects the panel to the decks; called once by [`super::wire`].
pub(super) fn wire(app: &App, decks: &Decks) {
    let c = prefs::with(|p| p.controller.clone());
    let opts = Opts::of(&c);
    let model = Rc::new(VecModel::from(vec![MixDeckView::default(); DECKS]));
    let st = app.global::<MixPointState>();
    st.set_decks(ModelRc::from(model.clone()));
    let mix = Rc::new(Mix {
        s: RefCell::new(Session::new(opts.default)),
        decks: decks.clone(),
        app: app.as_weak(),
        opts: Cell::new(opts),
        view: Cell::new(View::default()),
        ext: Cell::new((0.0, 0.0)),
        sizes: Cell::new([(0.0, 0.0); 3]),
        serial: Default::default(),
        cutting: Default::default(),
        forced: Default::default(),
        drawn: Cell::new([0; KEY]),
        model,
        timer: Timer::default(),
    });
    let weak = Rc::downgrade(&mix);
    mix.timer.start(TimerMode::Repeated, TICK, move || {
        if let Some(m) = weak.upgrade() {
            m.tick();
        }
    });
    on!(st, mix, on_set_mode, |m, n| m.set_mode(n));
    on!(st, mix, on_select_deck, |m, d| m.select_deck(d as usize));
    on!(st, mix, on_pick_cue, |m| m.pick_cue());
    on!(st, mix, on_silent_toggled, |m| m.silent_toggled());
    on!(st, mix, on_silent_edit_toggled, |m| m.silent_edit_toggled());
    on!(st, mix, on_link, |m| m.link());
    on!(st, mix, on_clear, |m, which| m.clear(which));
    on!(st, mix, on_wave_click, |m, d, f| m
        .wave_click(d as usize, f));
    on!(st, mix, on_resized, |m, which, w, h| m.resized(
        which as usize,
        w,
        h
    ));
    on!(st, mix, on_lane_drag, |m, dx| m.lane_drag(dx));
    on!(st, mix, on_overview_move, |m, f| m.overview_move(f));
    on!(st, mix, on_zoom, |m, steps| m.zoom(steps));
    let weak = Rc::downgrade(&mix);
    prefs::on_change(move |p| {
        if let Some(m) = weak.upgrade() {
            m.prefs_changed(&p.controller);
        }
    });
    mix.prefs_changed(&c);
}

impl Mix {
    fn say(&self, text: &str) {
        if let Some(app) = self.app.upgrade() {
            toast::show(&app, text);
        }
    }

    fn prefs_changed(&self, c: &Controller) {
        let (old, new) = (self.opts.get(), Opts::of(c));
        self.opts.set(new);
        if new.enabled && !self.timer.running() {
            self.timer.restart();
        }
        if old.default != new.default {
            self.s.borrow_mut().mode = new.default;
        }
        if let Some(app) = self.app.upgrade() {
            let st = app.global::<MixPointState>();
            st.set_enabled(new.enabled);
            if !new.enabled {
                st.set_open(false);
            }
        }
    }

    /// Whether `deck` may be picked in `mode`: MIX OUT from the playing deck or one a link starts, MIX IN from a stopped
    /// deck.
    fn eligible(&self, mode: Mode, deck: usize) -> Result<(), &'static str> {
        let p = probe(&self.decks[deck]);
        if !p.ready {
            return Err("MIX POINT LINK: load a track on that deck first");
        }
        let started_by_link = self.s.borrow().link.contains(&Some(deck));
        match mode {
            Mode::Out if !p.playing && !started_by_link => Err("MIX OUT: pick the playing deck"),
            Mode::In if p.playing => Err("MIX IN: pick a stopped deck"),
            _ => Ok(()),
        }
    }

    /// A deck whose BEAT SYNC was turned into BPM SYNC for the mix gets its own setting back.
    fn restore_sync(&self, d: usize) {
        if self.forced[d].take() {
            prefs::with(|p| self.decks[d].sync.refresh(&p.controller));
        }
    }

    fn tick(&self) {
        let o = self.opts.get();
        if !o.enabled {
            self.release_all();
            // Off: nothing is held any more, so the timer sleeps until `prefs_changed` switches the function on.
            self.timer.stop();
            return;
        }
        self.forget_reloaded();
        self.apply_silent();
        self.run_links(o);
        self.render();
    }

    /// The function is off: nobody stays muted or on BPM SYNC because of it.
    fn release_all(&self) {
        for d in 0..DECKS {
            if self.cutting[d].get() {
                self.set_cut(d, false);
            }
            self.restore_sync(d);
        }
    }

    /// A new track on a deck: its points, links and SILENT go.
    fn forget_reloaded(&self) {
        for (d, deck) in self.decks.iter().enumerate() {
            let serial = deck.load_serial.get();
            if self.serial[d].replace(serial) != serial {
                self.s.borrow_mut().forget(d);
                self.restore_sync(d);
            }
        }
    }

    fn set_cut(&self, d: usize, on: bool) {
        self.cutting[d].set(on);
        self.decks[d].with_player(|p| p.set_cut(on));
    }

    /// SILENT: the MIX IN deck is muted from the beginning of its track to the silent end, whoever plays it.
    fn apply_silent(&self) {
        for d in 0..DECKS {
            let until = self.s.borrow().silent_until(d);
            let want = until.is_some_and(|end| {
                let p = probe(&self.decks[d]);
                p.ready && plan::muted(p.pos, end)
            });
            // While muted the cut is set again every tick (the JOG CUTTER shares the player's cut and may have let go of it).
            if want || self.cutting[d].get() {
                self.set_cut(d, want);
            }
        }
    }

    /// Every armed link: starts its MIX IN deck when the MIX OUT deck is due, and re-arms it once the OUT deck went back.
    fn run_links(&self, o: Opts) {
        for out in 0..DECKS {
            let (inn, out_ms, in_ms, fired) = {
                let s = self.s.borrow();
                let Some(inn) = s.link[out] else { continue };
                let (Some(out_ms), Some(in_ms)) = (s.out_ms[out], s.in_ms[inn]) else {
                    continue;
                };
                (inn, f64::from(out_ms), f64::from(in_ms), s.fired[out])
            };
            let (po, pi) = (probe(&self.decks[out]), probe(&self.decks[inn]));
            if !po.ready || !pi.ready {
                continue;
            }
            let lead = plan::in_position(po.pos, out_ms, po.rate, in_ms, pi.rate);
            if fired {
                if lead < -1000.0 {
                    self.s.borrow_mut().fired[out] = false;
                }
                if !pi.playing {
                    self.restore_sync(inn);
                }
                continue;
            }
            if !po.playing {
                continue;
            }
            if pi.playing {
                self.s.borrow_mut().fired[out] = true; // someone else started it
                continue;
            }
            if let Some(at) = plan::start_at(po.pos, out_ms, po.rate, in_ms, pi.rate, pi.duration) {
                self.start(inn, at, o);
                self.s.borrow_mut().fired[out] = true;
            }
        }
    }

    /// The MIX IN deck starts at `at` ms; "Automatically changes the BEAT/BPM SYNC setting" turns a synced deck whose
    /// grid drifts to BPM SYNC for the mix.
    fn start(&self, d: usize, at: u32, o: Opts) {
        let deck = &self.decks[d];
        let drifting = plan::inconsistent_bpm(&deck.loaded.borrow().analysis.beats);
        if o.auto_sync && drifting && deck.is_synced() {
            deck.force_bpm_sync();
            self.forced[d].set(true);
        }
        deck.with_player(|p| {
            p.seek(at);
            p.play();
        });
    }
}
