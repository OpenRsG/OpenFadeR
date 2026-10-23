//! The tick-driven side of Automix: the state machine ([`Automix::step`]), arming and finishing a mix, the crossfader
//! and bass swap, the tempo glide and the status line.

use super::{Automix, Run, Stage, next_deck, plan};
use crate::app::{App, AutomixState, BrowserState, MixerState, deck};
use slint::ComponentHandle;

/// Tempo ratio moved per tick while gliding back to 1.0 (a 10 % offset takes 4 s).
const GLIDE_STEP: f64 = 0.0005;

/// Track time left at which a track counts as ended.
fn at_end(pos: f64, dur: f64) -> bool {
    dur > 0.0 && pos + 50.0 >= dur
}

impl Automix {
    fn pos(&self, d: usize) -> (f64, bool, f64) {
        self.decks[d]
            .with_player(|p| (p.position_ms(), p.is_playing(), p.duration_ms() as f64))
            .unwrap_or_default()
    }

    /// Crossfader travel towards the incoming deck, constant power.
    fn drive(app: &App, _out: usize, inc: usize, x: f64) {
        let s = app.global::<MixerState>();
        let pos = if inc % 2 == 1 {
            x as f32
        } else {
            1.0 - x as f32
        };
        s.set_auto_fade(true);
        s.set_crossfader(pos);
        s.invoke_crossfade(pos);
    }

    pub(super) fn release(app: &App) {
        let s = app.global::<MixerState>();
        s.set_auto_fade(false);
        s.invoke_crossfade(s.get_crossfader());
    }

    /// The incoming deck takes over completely: crossfader on it, the outgoing deck pauses.
    pub(super) fn finish(&self, run: &mut Run, app: &App, out: usize, inc: usize) -> Stage {
        self.decks[out].with_player(|p| p.pause());
        Self::drive(app, out, inc, 1.0);
        if let Some(f) = run.fade.take() {
            f.end();
        }
        Self::release(app);
        self.glide.set(Some((inc, self.decks[inc].rate())));
        Stage::OnAir { cur: Some(inc) }
    }

    /// One glide step for the deck that just took over; it ends at 1.0, on a hand-moved tempo, or when the next mix arms.
    fn glide_step(&self, stage: &Stage) {
        let Some((d, last)) = self.glide.get() else {
            return;
        };
        let deck = &self.decks[d];
        let done = !matches!(stage, Stage::OnAir { .. } | Stage::Loading { .. })
            || (deck.rate() - last).abs() > 1e-9;
        let next = plan::glide(last, GLIDE_STEP);
        if !done {
            deck.set_rate(next);
        }
        self.glide.set((!done && next != 1.0).then_some((d, next)));
    }

    /// Parks the loaded `inc` for the outgoing `cur`: tempo matched, at the mix-in point, plan made.
    pub(super) fn arm(
        &self,
        run: &mut Run,
        app: &App,
        cur: usize,
        inc: usize,
        serial: u32,
        asap: bool,
    ) -> Stage {
        let (a, b) = (&self.decks[cur], &self.decks[inc]);
        let ra = a.rate();
        let (pos, ..) = self.pos(cur);
        let recipe = &super::style::all()[super::style::chosen(run)];
        let bars = recipe
            .bars
            .unwrap_or(app.global::<AutomixState>().get_bars().max(1) as u32);
        let rushed = asap || run.asap;
        let plan = a.with_analysis(|aa, dur| {
            b.with_analysis(|ba, _| plan::plan(aa, dur, ba, bars, pos, rushed))
        });
        run.rushed = rushed;
        run.asap = false;
        let rb = self.tempo_for(cur, inc, pos).unwrap_or(1.0);
        b.set_rate(rb);
        if let Some(p) = plan {
            b.with_player(|pl| pl.seek(p.in_ms.max(0.0) as u32));
        }
        Stage::Armed {
            cur,
            inc,
            serial,
            plan,
            ra,
            rb,
            grids: self.grids(cur, inc),
        }
    }

    /// Both decks hold a track with a beat grid.
    fn grids(&self, cur: usize, inc: usize) -> bool {
        let has = |d: usize| {
            self.decks[d].with_analysis(|a, _| crate::app::deck::grid_bpm(&a.beats).is_some())
        };
        has(cur) && has(inc)
    }

    /// An `Armed` mix that was parked before the analysis of its tracks arrived (so with no plan and the tempo unmatched) is
    /// planned again once both grids are there; `None` while nothing changed.
    pub(super) fn replanned(
        &self,
        run: &mut Run,
        app: &App,
        cur: usize,
        inc: usize,
        serial: u32,
        grids: bool,
    ) -> Option<Stage> {
        (!grids && self.grids(cur, inc)).then(|| self.arm(run, app, cur, inc, serial, false))
    }

    /// Playback rate for `inc` to match `cur`'s tempo at its playhead (`pos`), `inc`'s at its mix-in anchor.
    fn tempo_for(&self, cur: usize, inc: usize, pos: f64) -> Option<f64> {
        let ra = self.decks[cur].rate();
        self.decks[cur].with_analysis(|aa, _| {
            self.decks[inc].with_analysis(|ba, _| {
                let out_bpm = deck::tempo_at(&aa.beats, pos)?;
                let anchor = plan::anchor(ba).unwrap_or(0.0);
                let inc_bpm = deck::tempo_at(&ba.beats, anchor)?;
                plan::rate_for(out_bpm, ra, inc_bpm)
            })
        })
    }

    pub(super) fn tick(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let stopped = {
            let mut guard = self.run.borrow_mut();
            let Some(run) = guard.as_mut() else { return };
            let stopped = self.step(&app, run);
            if run.plan_at != Some(run.next) {
                super::style::show(&app, run);
            }
            if stopped.is_none() {
                app.global::<AutomixState>()
                    .set_status(self.describe(run).into());
            }
            stopped
        };
        if let Some(why) = stopped {
            self.stop(&why);
        }
    }

    /// One tick of the state machine. `Some(reason)` stops Automix.
    fn step(&self, app: &App, run: &mut Run) -> Option<String> {
        self.glide_step(&run.stage);
        match run.stage {
            Stage::OnAir { cur } => {
                let inc = next_deck(cur, deck::active(), |d| self.decks[d].playing());
                if run.next >= run.queue.len() {
                    let ended = cur.is_none_or(|c| {
                        let (pos, playing, dur) = self.pos(c);
                        !playing && at_end(pos, dur)
                    });
                    return ended.then(|| tr!("AUTOMIX: end of list").to_owned());
                }
                if self.decks[inc].playing() {
                    return None; // played by hand: wait until it is free
                }
                self.decks[inc].load(&run.queue[run.next], &*self.lib);
                run.rushed = false;
                run.next += 1;
                let serial = self.decks[inc].serial().unwrap_or(0);
                run.stage = Stage::Loading { cur, inc, serial };
            }
            Stage::Loading { cur, inc, serial } => {
                let d = &self.decks[inc];
                if d.serial() != Some(serial) {
                    return Some(tr!("AUTOMIX stopped: a deck was loaded by hand").into());
                }
                if !d.ready() {
                    return None;
                }
                if d.with_player(|p| p.duration_ms()) == Some(0) {
                    let title = &run.queue[run.next - 1].title;
                    app.global::<BrowserState>().set_status(
                        tr!("AUTOMIX: skipped “{}” (its audio did not load)", title).into(),
                    );
                    run.stage = Stage::OnAir { cur };
                    return None;
                }
                run.stage = match cur {
                    None => {
                        d.with_player(|p| p.play());
                        Self::drive(app, inc ^ 1, inc, 1.0); // first track: its side of the crossfader, full
                        Self::release(app);
                        Stage::OnAir { cur: Some(inc) }
                    }
                    Some(c) => self.arm(run, app, c, inc, serial, false),
                };
            }
            Stage::Armed {
                cur,
                inc,
                serial,
                plan,
                ra,
                rb,
                grids,
            } => {
                if self.decks[inc].serial() != Some(serial) {
                    return Some(tr!("AUTOMIX stopped: a deck was loaded by hand").into());
                }
                if let Some(armed) = self.replanned(run, app, cur, inc, serial, grids) {
                    run.stage = armed;
                    return None;
                }
                let (pos, playing, dur) = self.pos(cur);
                let ra_now = self.decks[cur].rate();
                let rb_now = self.tempo_for(cur, inc, pos).unwrap_or(rb);
                if (ra_now - ra).abs() > 1e-6 || (rb_now - rb).abs() > 1e-6 {
                    // the outgoing deck's tempo moved (its rate fader, or a grid section change): follow it (plan times
                    // are on the outgoing track's own timeline)
                    self.decks[inc].set_rate(rb_now);
                    run.stage = Stage::Armed {
                        cur,
                        inc,
                        serial,
                        plan,
                        ra: ra_now,
                        rb: rb_now,
                        grids,
                    };
                    return None;
                }
                match plan {
                    Some(p) if !at_end(pos, dur) => {
                        if playing && pos >= p.out_ms {
                            let pos0 = plan::expected_in(&p, pos, ra, rb).max(0.0);
                            self.decks[inc].with_player(|pl| {
                                pl.seek(pos0 as u32);
                                pl.play();
                            });
                            let len = p.bars as f64 * 4.0;
                            let recipe = &super::style::all()[super::style::chosen(run)];
                            let mut fade = super::fade::Fade::new(
                                recipe.timeline(len),
                                ((pos - p.out_ms) / p.fade_ms * len).max(0.0),
                                super::rig::Live::new(app, self.decks.clone(), cur, inc),
                            );
                            fade.advance(0.0, p.fade_ms / len / ra / 1000.0);
                            run.fade = Some(fade);
                            run.clock = std::time::Instant::now();
                            run.stage = Stage::Fading {
                                out: cur,
                                inc,
                                plan: p,
                                ra,
                                rb,
                                pos0,
                                aligned: false,
                            };
                        }
                    }
                    _ if at_end(pos, dur) => {
                        // no plan (no grid, under a bar left) or the outgoing track ended first: a cut
                        self.decks[inc].with_player(|pl| {
                            if let Some(p) = plan {
                                pl.seek(p.in_ms.max(0.0) as u32);
                            }
                            pl.play();
                        });
                        run.stage = self.finish(run, app, cur, inc);
                    }
                    _ => {}
                }
            }
            Stage::Fading {
                out,
                inc,
                plan,
                ra,
                rb,
                pos0,
                aligned,
            } => {
                let dt = run.clock.elapsed().as_secs_f64();
                run.clock = std::time::Instant::now();
                let beat_secs = plan.fade_ms
                    / (plan.bars as f64 * 4.0)
                    / self.decks[out].rate().max(0.001)
                    / 1000.0;
                let done = run
                    .fade
                    .as_mut()
                    .is_none_or(|f| f.advance(dt / beat_secs, beat_secs));
                let x = run.fade.as_ref().map_or(1.0, |f| f.progress());
                let a_pos = plan.out_ms + plan.fade_ms * x;
                if !aligned {
                    let (b_pos, ..) = self.pos(inc);
                    if b_pos - pos0 >= 1.0 {
                        // incoming deck is audible now: correct the output latency once, while it is still silent
                        if let Some(t) =
                            plan::align_target(b_pos, plan::expected_in(&plan, a_pos, ra, rb))
                        {
                            self.decks[inc].with_player(|p| p.seek(t));
                        }
                        run.stage = Stage::Fading {
                            out,
                            inc,
                            plan,
                            ra,
                            rb,
                            pos0,
                            aligned: true,
                        };
                    }
                }
                if done {
                    run.stage = self.finish(run, app, out, inc);
                }
            }
        }
        None
    }

    /// Status line text for the current stage.
    fn describe(&self, run: &Run) -> String {
        let incoming = run
            .next
            .checked_sub(1)
            .and_then(|i| run.queue.get(i))
            .map_or("", |t| t.title.as_str());
        match run.stage {
            Stage::OnAir { .. } => tr!(
                "AUTOMIX · on air · {} left in the list",
                run.queue.len() - run.next
            ),
            Stage::Loading { .. } => tr!("AUTOMIX · loading “{}”", incoming),
            Stage::Armed {
                cur,
                plan: Some(p),
                ra,
                ..
            } => {
                let s = ((p.out_ms - self.pos(cur).0) / ra / 1000.0).max(0.0) as u32;
                tr!(
                    "AUTOMIX · {} · “{}” mixes in over {} bars in {}",
                    super::style::all()[super::style::chosen(run)].name,
                    incoming,
                    p.bars,
                    format!("{}:{:02}", s / 60, s % 60)
                )
            }
            Stage::Armed { plan: None, .. } => {
                tr!(
                    "AUTOMIX · “{}” cuts in when this track ends (no grid or no room)",
                    incoming
                )
            }
            Stage::Fading { .. } => {
                let x = run.fade.as_ref().map_or(1.0, |f| f.progress());
                tr!(
                    "AUTOMIX · {} · mixing “{}” {}",
                    super::style::all()[super::style::chosen(run)].name,
                    incoming,
                    format!("{:.0}%", x * 100.0)
                )
            }
        }
    }
}
