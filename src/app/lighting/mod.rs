//! LIGHTING mode ↔ ui/lighting.slint (global `LightingState`); the lights themselves are `openfader::lighting`.
//! A 25 ms timer (40 Hz, the usual DMX refresh) takes the master deck's beat position and phrase ([`Deck::light_input`]:
//! the `DeckState.master` deck while it plays, else the first playing deck), computes every fixture's look, shows it on the
//! stage simulator and sends the universes as Art-Net / sACN to the configured IP. With no deck playing the beat runs free at
//! 120 BPM so the stage keeps its ambient glow. The timer runs whatever mode is on screen: the lights follow the decks in
//! PERFORMANCE too. Settings and patch are saved on every change (`lighting.json` in the config dir).
//!
//! Children, all `impl Lighting`: `control` (mode / strobe / palette chips), `output` (UDP output, status line, saving),
//! `patch` (fixture rows), `tick` (the 40 Hz loop).

mod control;
mod output;
mod patch;
mod tick;

use super::{App, FixtureView, LightingState, deck::Decks, prefs};
use control::{mode_of, strobe_of};
use openfader::lighting::{
    Control, Profile,
    config::{self, Config},
    fixtures,
    output::{Output, Protocol},
    scene::Palette,
};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

pub struct Lighting {
    app: slint::Weak<App>,
    decks: Decks,
    profiles: Vec<Profile>,
    cfg: RefCell<Config>,
    ctl: Cell<Control>,
    /// The profile ADD FIXTURE patches.
    pick: Cell<usize>,
    out: RefCell<Option<Output>>,
    /// Why the output is not running ("" = fine).
    out_err: RefCell<String>,
    model: Rc<VecModel<FixtureView>>,
    started: Instant,
    _timer: Timer,
}

/// Position of `v` in a UI chip list (0 when absent).
fn index_of<T: PartialEq>(all: &[T], v: &T) -> i32 {
    all.iter().position(|p| p == v).unwrap_or(0) as i32
}

pub fn wire(app: &App, decks: Decks) -> Rc<Lighting> {
    let (profiles, notes) = fixtures::all();
    notes
        .iter()
        .for_each(|n| eprintln!("lighting: fixture file skipped: {n}"));
    let cfg = config::path().map_or_else(
        || Config::starter(&profiles),
        |p| config::load(&p, &profiles),
    );
    let ctl = Control {
        palette: cfg.palette,
        ..Control::default()
    };
    let l = Rc::new(Lighting {
        app: app.as_weak(),
        decks,
        profiles,
        ctl: Cell::new(ctl),
        cfg: RefCell::new(cfg),
        pick: Cell::new(0),
        out: RefCell::new(None),
        out_err: RefCell::default(),
        model: Rc::new(VecModel::default()),
        started: Instant::now(),
        _timer: Timer::default(),
    });
    let st = app.global::<LightingState>();
    st.set_fixtures(ModelRc::from(l.model.clone()));
    st.set_fixtures_dir(
        fixtures::dir()
            .map_or(String::new(), |d| d.display().to_string())
            .into(),
    );
    st.set_ip(l.cfg.borrow().ip.clone().into());
    l.reopen();
    l.show_patch();
    l.show_controls();
    l.show_profile();

    on!(st, weak l, on_set_protocol, |l, i| {
        l.cfg.borrow_mut().protocol = Protocol::ALL[i.clamp(0, 3) as usize];
        l.changed();
        l.reopen();
    });
    on!(st, weak l, on_set_ip, |l, ip| {
        l.cfg.borrow_mut().ip = ip.trim().to_owned();
        l.changed();
        l.reopen();
    });
    on!(st, weak l, on_set_palette, |l, i| {
        let p = Palette::ALL[i.clamp(0, 7) as usize];
        l.cfg.borrow_mut().palette = p;
        l.update_control(|c| c.palette = p);
        l.changed();
    });
    on!(st, weak l, on_set_mode, |l, i| l.update_control(|c| c.mode = mode_of(i)));
    on!(st, weak l, on_set_strobe, |l, i| l.update_control(|c| c.strobe = strobe_of(i)));
    on!(st, weak l, on_set_master, |l, v| l.update_control(|c| c.master = v.clamp(0.0, 1.0)));
    on!(st, weak l, on_step_profile, |l, d| {
        l.pick
            .set((l.pick.get() as i32 + d).rem_euclid(l.profiles.len() as i32) as usize);
        l.show_profile();
    });
    on!(st, weak l, on_add_fixture, |l| {
        let added = l.cfg.borrow_mut().rig.add(&l.profiles[l.pick.get()]);
        if added.is_none() {
            l.say("that profile does not fit a universe");
        }
        l.patch_changed();
    });
    on!(st, weak l, on_remove_fixture, |l, i| {
        let mut c = l.cfg.borrow_mut();
        if (i as usize) < c.rig.fixtures.len() {
            c.rig.fixtures.remove(i as usize);
        }
        drop(c);
        l.patch_changed();
    });
    on!(st, weak l, on_set_address, |l, i, u, a| {
        let r = l.cfg.borrow_mut().rig.set_address(
            i as usize,
            u.clamp(0, 32767) as u16,
            a.clamp(0, 512) as u16,
        );
        if let Err(e) = r {
            l.say(&e);
        }
        l.patch_changed();
    });

    let w = Rc::downgrade(&l);
    l._timer.start(TimerMode::Repeated, tick::TICK, move || {
        if let Some(l) = w.upgrade() {
            l.tick();
        }
    });
    l.set_enabled(prefs::with(|p| p.extensions.lighting));
    let w = Rc::downgrade(&l);
    prefs::on_change(move |p| {
        if let Some(l) = w.upgrade() {
            l.set_enabled(p.extensions.lighting);
        }
    });
    l
}

impl Lighting {
    fn state<R>(&self, f: impl FnOnce(LightingState<'_>) -> R) -> Option<R> {
        self.app.upgrade().map(|a| f(a.global::<LightingState>()))
    }

    /// EXTENSIONS > Lighting: off hides the view and stops the tick from driving the lights.
    fn set_enabled(&self, on: bool) {
        self.state(|s| {
            s.set_enabled(on);
            if !on {
                s.set_visible(false);
            }
        });
    }

    fn say(&self, text: &str) {
        self.out_err.replace(text.to_owned());
        self.show_status(0);
    }
}
