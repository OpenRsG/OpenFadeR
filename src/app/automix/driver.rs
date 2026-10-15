//! The tick-driven side of Automix: the state machine ([`Automix::step`]), arming and finishing a mix, the crossfader
//! and bass swap, the tempo glide and the status line.

use super::{Automix, Run, Stage, next_deck, plan};
use crate::app::{App, AutomixState, BrowserState, MixerState, deck::active};
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

    /// Crossfader, auto-fade flag and bass swap for fade progress `x` towards deck `inc`.
    fn drive(app: &App, out: usize, inc: usize, x: f64) {
        let m = plan::mix_at(x);
        let s = app.global::<MixerState>();
        let pos = if inc % 2 == 1 {
            m.travel
        } else {
            1.0 - m.travel
        };
        s.set_auto_fade(true);
        s.set_auto_low_cut(i32::from(m.low_cut_in) << inc | i32::from(m.low_cut_out) << out);
        s.set_crossfader(pos);
        s.invoke_crossfade(pos); // the mixer re-applies every channel
    }

    /// Hands the crossfader and EQ back to the user's own settings.
    pub(super) fn release(app: &App) {
        let s = app.global::<MixerState>();
        s.set_auto_fade(false);
        s.set_auto_low_cut(0);
        s.invoke_crossfade(s.get_crossfader());
    }

    /// The incoming deck takes over completely: crossfader on it, the outgoing deck pauses.
    pub(super) fn finish(&self, app: &App, out: usize, inc: usize) -> Stage {
        self.decks[out].with_player(|p| p.pause());
        Self::drive(app, out, inc, 1.0);
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
        let bars = app.global::<AutomixState>().get_bars().max(1) as u32;
        let plan = a.with_analysis(|aa, dur| {
            b.with_analysis(|ba, _| plan::plan(aa, dur, ba, bars, pos, asap || run.asap))
        });
        run.asap = false;
        let rb = self.tempo_for(cur, inc).unwrap_or(1.0);
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
        }
    }

    fn tempo_for(&self, cur: usize, inc: usize) -> Option<f64> {
        let ra = self.decks[cur].rate();
        self.decks[cur].with_analysis(|aa, _| {
            self.decks[inc].with_analysis(|ba, _| plan::rate_for(&aa.beats, ra, &ba.beats))
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
                let inc = next_deck(cur, active(), |d| self.decks[d].playing());
                if run.next >= run.queue.len() {
                    let ended = cur.is_none_or(|c| {
                        let (pos, playing, dur) = self.pos(c);
                        !playing && at_end(pos, dur)
                    });
                    return ended.then(|| "AUTOMIX: end of list".to_owned());
                }
                if self.decks[inc].playing() {
                    return None; // played by hand: wait until it is free
                }
                self.decks[inc].load(&run.queue[run.next], &*self.lib);
                run.next += 1;
                let serial = self.decks[inc].serial().unwrap_or(0);
                run.stage = Stage::Loading { cur, inc, serial };
            }
            Stage::Loading { cur, inc, serial } => {
                let d = &self.decks[inc];
                if d.serial() != Some(serial) {
                    return Some("AUTOMIX stopped: a deck was loaded by hand".into());
                }
                if !d.ready() {
                    return None;
                }
                if d.with_player(|p| p.duration_ms()) == Some(0) {
                    let title = &run.queue[run.next - 1].title;
                    app.global::<BrowserState>().set_status(
                        format!("AUTOMIX: skipped “{title}” (its audio did not load)").into(),
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
            } => {
                if self.decks[inc].serial() != Some(serial) {
                    return Some("AUTOMIX stopped: a deck was loaded by hand".into());
                }
                let (pos, playing, dur) = self.pos(cur);
                let ra_now = self.decks[cur].rate();
                if (ra_now - ra).abs() > 1e-6 {
                    // the playing deck's tempo moved: follow it (plan times are on the outgoing track's own timeline)
                    let rb = self.tempo_for(cur, inc).unwrap_or(1.0);
                    self.decks[inc].set_rate(rb);
                    run.stage = Stage::Armed {
                        cur,
                        inc,
                        serial,
                        plan,
                        ra: ra_now,
                        rb,
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
                        run.stage = self.finish(app, cur, inc);
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
                let (a_pos, ..) = self.pos(out);
                let x = ((a_pos - plan.out_ms) / plan.fade_ms).clamp(0.0, 1.0);
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
                if x >= 1.0 {
                    run.stage = self.finish(app, out, inc);
                } else {
                    Self::drive(app, out, inc, x);
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
            Stage::OnAir { .. } => format!(
                "AUTOMIX · on air · {} left in the list",
                run.queue.len() - run.next
            ),
            Stage::Loading { .. } => format!("AUTOMIX · loading “{incoming}”"),
            Stage::Armed {
                cur,
                plan: Some(p),
                ra,
                ..
            } => {
                let s = ((p.out_ms - self.pos(cur).0) / ra / 1000.0).max(0.0) as u32;
                format!(
                    "AUTOMIX · “{incoming}” mixes in over {} bars in {}:{:02}",
                    p.bars,
                    s / 60,
                    s % 60
                )
            }
            Stage::Armed { plan: None, .. } => {
                format!("AUTOMIX · “{incoming}” cuts in when this track ends (no grid or no room)")
            }
            Stage::Fading { out, plan, .. } => {
                let x = ((self.pos(out).0 - plan.out_ms) / plan.fade_ms).clamp(0.0, 1.0);
                format!("AUTOMIX · mixing “{incoming}” {:.0}%", x * 100.0)
            }
        }
    }
}
