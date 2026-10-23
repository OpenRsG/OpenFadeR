//! Automix (`AutomixState` in ui/browser.slint): plays a list in order over the decks (A and B, or all four in the 4-deck layout: the next
//! track goes to the first free deck on the other crossfader side, [`next_deck`]). The driver
//! ticks on a UI timer ([`TICK`]) and walks one [`Stage`] at a time:
//! `OnAir` (one deck plays, nothing queued) → `Loading` (next track on the idle deck, [`Deck::load`])
//! → `Armed` (tempo synced to the playing deck, parked on its mix-in point, waiting for the outgoing
//! deck to reach the mix-out point) → `Fading` (incoming started, crossfader and bass swap driven over
//! N bars) → `OnAir` on the other deck. All decisions about *where* come from the pure [`plan`] child; the
//! per-tick state machine is the child `driver`.
//!
//! The crossfader is driven through `MixerState` (`crossfader`, `auto-fade`: constant-power gains whatever
//! the curve, `auto-low-cut`: the bass swap), so the mixer UI shows it moving. Stopping or finishing
//! releases both. Tracks that fail to decode are skipped; a track without a beat grid (or less than one bar
//! left) is *cut* in at the end of the outgoing one, with no fade.
//! The incoming deck is synced for the mix; once it is on air alone it glides back to its own tempo
//! ([`plan::glide`], `driver::GLIDE_STEP` per tick). Without SYNC each track plays at its original
//! tempo, so the sync is only a mixing aid. With key lock
//! off the pitch follows the glide; a hand-moved tempo cancels it.
//! ponytail: the crossfader jumps to the outgoing side when a fade starts; a glide still running when
//! the next mix arms is abandoned where it is. Automix puts the channels back on the crossfader sides A, B, A, B (decks A and C on
//! A, B and D on B) while it runs; the user's THRU / swapped assignments are not restored.

mod driver;
mod plan;

use super::{
    App, AutomixState, BrowserState, DeckState, MixerState,
    deck::{DECKS, Decks, active},
};
use plan::Plan;
use openfader::library::{Library, Track};
use slint::{ComponentHandle, Model, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::Duration,
};

const TICK: Duration = Duration::from_millis(20);

#[derive(Clone, Copy)]
enum Stage {
    /// `cur` is playing (`None`: nothing has started yet); the next queued track is loaded on the other deck.
    OnAir { cur: Option<usize> },
    /// The next track decodes on `inc`; `serial` is its load, so a manual reload is noticed.
    Loading {
        cur: Option<usize>,
        inc: usize,
        serial: u32,
    },
    /// `inc` is ready, at tempo `rb` (outgoing at `ra`), parked on the plan's mix-in point. `plan: None` = cut at the end.
    Armed {
        cur: usize,
        inc: usize,
        serial: u32,
        plan: Option<Plan>,
        ra: f64,
        rb: f64,
        /// Both tracks had a beat grid when it was planned (else it is planned again once the analysis delivers them).
        grids: bool,
    },
    /// `inc` started at `pos0`; `aligned` once its beat phase was corrected against the outgoing deck.
    Fading {
        out: usize,
        inc: usize,
        plan: Plan,
        ra: f64,
        rb: f64,
        pos0: f64,
        aligned: bool,
    },
}

struct Run {
    queue: Vec<Track>,
    /// Index of the next track to load.
    next: usize,
    /// SKIP: mix at the next bar line instead of the planned point.
    asap: bool,
    stage: Stage,
}

pub struct Automix {
    app: slint::Weak<App>,
    decks: Decks,
    lib: Rc<dyn Library>,
    run: RefCell<Option<Run>>,
    timer: Timer,
    /// Deck gliding back to its own tempo, and the rate we last set on it (a different one = the user moved it).
    glide: Cell<Option<(usize, f64)>>,
}

/// Connects `AutomixState.skip` and starts the driver timer. The browser owns the AUTOMIX button (it
/// knows the list) and calls [`Automix::start`] / [`Automix::stop`].
pub fn wire(app: &App, decks: Decks, lib: Rc<dyn Library>) -> Rc<Automix> {
    let am = Rc::new(Automix {
        app: app.as_weak(),
        decks,
        lib,
        run: RefCell::new(None),
        timer: Timer::default(),
        glide: Cell::new(None),
    });
    let a = am.clone();
    app.global::<AutomixState>().on_skip(move || a.skip());
    let weak: Weak<Automix> = Rc::downgrade(&am);
    am.timer.start(TimerMode::Repeated, TICK, move || {
        if let Some(a) = weak.upgrade() {
            a.tick();
        }
    });
    // Idle until `start` (the tick returns at once without a run): no 50 Hz wake-ups while Automix is off.
    am.timer.stop();
    am
}

/// The crossfader assignment (`ChannelView.xf`) of deck `d` under Automix: 0 = A side for decks A and C, 2 = B side for B and D.
fn side_of(d: usize) -> i32 {
    if d % 2 == 0 { 0 } else { 2 }
}

/// The deck the next track loads on, with `shown` decks in use and `cur` on air: the first one on the other crossfader side
/// (odd after even, even after odd) that is not already playing by hand, in order after `cur`; all of them busy gives the
/// first of them (the caller then waits). Nothing on air: the first idle deck, else A.
fn next_deck(cur: Option<usize>, shown: usize, playing: impl Fn(usize) -> bool) -> usize {
    let Some(c) = cur else {
        return (0..shown).find(|&d| !playing(d)).unwrap_or(0);
    };
    let opposite: Vec<usize> = (1..shown)
        .map(|k| (c + k) % shown)
        .filter(|d| d % 2 != c % 2)
        .collect();
    opposite
        .iter()
        .copied()
        .find(|&d| !playing(d))
        .or(opposite.first().copied())
        .unwrap_or(c ^ 1)
}

impl Automix {
    pub fn running(&self) -> bool {
        self.run.borrow().is_some()
    }

    /// Plays `queue` in order. A deck that is already playing counts as the track on air (`queue[0]` is
    /// then the first track mixed in); otherwise `queue[0]` starts on the first idle deck.
    pub fn start(&self, queue: Vec<Track>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let say = |t: &str| app.global::<BrowserState>().set_status(t.into());
        if queue.is_empty() {
            return say(tr!("AUTOMIX: the list is empty"));
        }
        if self.decks.iter().any(|d| d.with_player(|_| ()).is_none()) {
            return say(tr!("AUTOMIX: no audio output"));
        }
        let (view, master) = (
            app.global::<DeckState>().get_decks(),
            app.global::<DeckState>().get_master(),
        );
        let playing = |i| view.row_data(i).is_some_and(|d| d.playing);
        let cur = usize::try_from(master)
            .ok()
            .filter(|&m| m < active() && playing(m))
            .or_else(|| (0..active()).find(|&i| playing(i)));
        let m = app.global::<MixerState>();
        let channels = m.get_channels();
        for i in 0..DECKS {
            if let Some(mut c) = channels.row_data(i) {
                c.xf = side_of(i);
                channels.set_row_data(i, c);
            }
        }
        m.invoke_crossfade(m.get_crossfader());
        let n = queue.len();
        *self.run.borrow_mut() = Some(Run {
            queue,
            next: 0,
            asap: false,
            stage: Stage::OnAir { cur },
        });
        self.timer.restart();
        let s = app.global::<AutomixState>();
        s.set_running(true);
        s.set_status(tr!("AUTOMIX · {} tracks", n).into());
    }

    /// Stops the driver and hands the mixer back; the decks keep playing. `why` goes to the browser status line.
    pub fn stop(&self, why: &str) {
        if self.run.borrow_mut().take().is_none() {
            return;
        }
        self.timer.stop();
        if let Some(app) = self.app.upgrade() {
            Self::release(&app);
            let s = app.global::<AutomixState>();
            s.set_running(false);
            s.set_status("".into());
            app.global::<BrowserState>().set_status(why.into());
        }
    }

    /// Skip to next: mixes the next track in at the next bar line; during a fade, finishes it now.
    fn skip(&self) {
        let (Some(app), mut guard) = (self.app.upgrade(), self.run.borrow_mut()) else {
            return;
        };
        let Some(run) = guard.as_mut() else { return };
        match run.stage {
            Stage::Fading { out, inc, .. } => run.stage = self.finish(&app, out, inc),
            Stage::Armed {
                cur, inc, serial, ..
            } => {
                run.stage = match self.arm(run, &app, cur, inc, serial, true) {
                    // no bar line to mix at (no grid, or under a bar left): the next track comes in now
                    Stage::Armed { plan: None, .. } => {
                        self.decks[inc].with_player(|p| p.play());
                        self.finish(&app, cur, inc)
                    }
                    armed => armed,
                }
            }
            Stage::Loading { cur: Some(_), .. } => run.asap = true,
            Stage::OnAir { cur: Some(_) } if run.next < run.queue.len() => run.asap = true,
            _ => app
                .global::<BrowserState>()
                .set_status(tr!("AUTOMIX: no next track to skip to").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_decks_alternate() {
        let idle = |_| false;
        assert_eq!([Some(0), Some(1)].map(|c| next_deck(c, 2, idle)), [1, 0]);
        assert_eq!(next_deck(None, 2, idle), 0);
        assert_eq!(
            next_deck(None, 2, |d| d == 0),
            1,
            "the first idle deck starts the list"
        );
    }

    #[test]
    fn four_decks_go_to_the_free_deck_on_the_other_crossfader_side() {
        let idle = |_| false;
        // A on air → B (odd side) first, then D; B → C then A; C → D then B; D → A then C
        assert_eq!(
            [0, 1, 2, 3].map(|c| next_deck(Some(c), 4, idle)),
            [1, 2, 3, 0]
        );
        // B is busy playing by hand: A's next track goes to D instead
        assert_eq!(next_deck(Some(0), 4, |d| d == 1), 3);
        // every deck on the other side busy: the first of them (the caller waits for it)
        assert_eq!(next_deck(Some(0), 4, |d| d == 1 || d == 3), 1);
        // the next deck is always on the opposite side, so the crossfader fade has somewhere to go
        for c in 0..4 {
            assert_ne!(side_of(next_deck(Some(c), 4, idle)), side_of(c));
        }
        // the 2-deck layout never reaches C or D
        assert_eq!(next_deck(Some(1), 2, |d| d == 0), 0);
    }

    /// Two decks and the driver, no audio and no library store.
    fn rig() -> (App, Decks, Rc<Automix>) {
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(openfader::library::own::Own::new(std::sync::Arc::new(
            openfader::store::Store::in_memory().unwrap(),
        )));
        let decks = crate::app::deck::wire(&app, lib.clone(), Default::default());
        let am = wire(&app, decks.clone(), lib);
        (app, decks, am)
    }

    /// 5 min at 120 BPM, 4/4.
    fn grid() -> openfader::analysis::Analysis {
        let beats = (0..600)
            .map(|i| openfader::analysis::Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect();
        openfader::analysis::Analysis {
            beats,
            ..Default::default()
        }
    }

    fn parked(plan: Option<Plan>, grids: bool) -> Run {
        Run {
            queue: vec![],
            next: 0,
            asap: false,
            stage: Stage::Armed {
                cur: 0,
                inc: 1,
                serial: 0,
                plan,
                ra: 1.0,
                rb: 1.0,
                grids,
            },
        }
    }

    #[test]
    fn a_mix_parked_before_the_analysis_arrived_is_planned_when_the_grids_are_there() {
        let (app, decks, am) = rig();
        decks[0].install_for_test(grid());
        decks[1].install_for_test(grid());
        let mut run = parked(None, false);
        let armed = am.replanned(&mut run, &app, 0, 1, 0, false);
        assert!(matches!(
            armed,
            Some(Stage::Armed {
                plan: Some(_),
                grids: true,
                ..
            })
        ));
        assert!(
            am.replanned(&mut run, &app, 0, 1, 0, true).is_none(),
            "a mix planned with its grids stays as it is"
        );
    }

    #[test]
    fn skip_without_a_plan_cuts_the_next_track_in_now() {
        let (_app, _decks, am) = rig(); // nothing loaded: no grid, so no bar line to mix at
        *am.run.borrow_mut() = Some(parked(None, false));
        am.skip();
        let run = am.run.borrow();
        assert!(matches!(
            run.as_ref().unwrap().stage,
            Stage::OnAir { cur: Some(1) }
        ));
    }
}
