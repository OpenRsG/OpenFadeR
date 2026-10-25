//! The deck's tick: picks up a finished decode, runs SYNC ([`Deck::follow`]), and redraws the time, the overview and the play
//! state only when something they show changed; the enlarged waveform, platter and phase meter go through [`Deck::scroll`]
//! ([`super::frame`], which owns their updates while a deck plays if the renderer supports it). Its period is VIEW > Waveform Drawing Rate
//! ([`period`]; the timer is created in `Deck::new` and re-timed by [`wire_view`]). Also the deck's VIEW consumers: the jog
//! display, SYNC RATE, click-on-waveform / PAD FX colour / PAD Mode Auto (pushed to `DeckState` by [`wire_view`]).

use super::{
    App, Deck, DeckState, Decks, at_end, beat_at, ending, fmt_ms, frame, prefs, render, sync,
    tempo_at,
};
use crate::app::prefs::view::{DrawRate, JogDisplay, PadMode};
use openfader::analysis::{Beat, Cue};
use serde_json::json;
use slint::{ComponentHandle, Model};
use std::{cell::Cell, time::Duration};

/// Time of one platter turn, 33⅓ rpm.
const LAP_MS: f64 = 1800.0;
/// How far ahead (laps) HOT CUE COUNTDOWN looks.
const COUNTDOWN_LAPS: f64 = 5.0;

/// What the per-tick code reads of the settings (a copy, so a tick does not clone the whole settings).
#[derive(Clone, Copy)]
pub(super) struct Opts {
    countdown: bool,
    sync_rate: bool,
    half_double: bool,
    pub(super) stems: bool,
    pub(super) stems_four: bool,
    pub(super) stem_wave: bool,
}

thread_local! {
    static OPTS: Cell<Opts> = const {
        Cell::new(Opts {
            countdown: false,
            sync_rate: true,
            half_double: true,
            stems: false,
            stems_four: true,
            stem_wave: false,
        })
    };
}

pub(super) fn opts() -> Opts {
    OPTS.get()
}

/// Period of the deck tick for a drawing rate: the waveforms and playheads redraw at most this often.
fn period(rate: DrawRate) -> Duration {
    Duration::from_millis(match rate {
        DrawRate::Fps30 => 33,
        DrawRate::Fps60 => 16,
        DrawRate::Fps120 => 8,
    })
}

/// Angle (degrees clockwise from the top) of the platter at track position `ms`.
pub(super) fn jog_angle(ms: f64) -> f32 {
    (ms / LAP_MS).rem_euclid(1.0) as f32 * 360.0
}

/// Laps from `pos` ms to the nearest hot cue ahead of it; `None` when there is none within [`COUNTDOWN_LAPS`].
fn laps_to_hot_cue(cues: &[Cue], pos: f64) -> Option<f32> {
    let ahead = cues
        .iter()
        .filter(|c| c.hot_cue > 0 && f64::from(c.time_ms) > pos)
        .map(|c| f64::from(c.time_ms) - pos);
    let laps = ahead.min_by(f64::total_cmp)? / LAP_MS;
    (laps <= COUNTDOWN_LAPS).then_some(laps as f32)
}

/// SYNC RATE text of the follower's multiple of the master's tempo.
fn rate_text(k: f64) -> &'static str {
    match k {
        2.0 => "×2",
        0.5 => "×½",
        _ => "×1",
    }
}

/// Pushes the VIEW settings this section consumes (now and on every change): the tick period, the click-on-waveform switch,
/// the PAD FX colour, PAD Mode Auto, and what the tick reads each frame.
pub(super) fn wire_view(app: &App, decks: &Decks) {
    let (weak, decks) = (app.as_weak(), decks.clone());
    let apply = move |p: &prefs::Prefs| {
        let Some(app) = weak.upgrade() else { return };
        let s = app.global::<DeckState>();
        s.set_click_wave(p.view.click_wave);
        s.set_pad_fx_color(p.view.pad_fx_color as i32);
        s.set_pad_auto(p.view.pad_mode == PadMode::Auto);
        OPTS.set(Opts {
            countdown: p.view.jog_display == JogDisplay::HotCueCountdown,
            sync_rate: p.view.deck_sync_rate,
            half_double: p.controller.sync_half_double,
            stems: p.stems,
            stems_four: p.stems_four,
            stem_wave: p.stem_wave,
        });
        let every = period(p.view.draw_rate);
        frame::set_rate(every);
        decks
            .iter()
            .filter(|d| d._timer.interval() != every)
            .for_each(|d| d._timer.set_interval(every));
    };
    apply(&prefs::get());
    prefs::on_change(apply);
}

/// Position in the bar (the grid's beats per bar), 0..1 (beat `bar_pos` of the nearest grid beat plus the fraction).
fn bar_phase(beats: &[Beat], ms: f64) -> Option<f64> {
    let b = beat_at(beats, ms)?;
    let i = (b.floor().max(0.0) as usize).min(beats.len() - 1);
    let per = f64::from(super::grid::per_bar(beats));
    Some((f64::from(beats[i].bar_pos.clamp(1, per as u16) - 1) + b - i as f64).rem_euclid(per) / per)
}

/// This deck's bar phase against the reference's, in beats of a `per`-beat bar, the short way round (±`per`/2; positive = ahead).
pub(super) fn phase_offset(own: f64, reference: f64, per: f64) -> f64 {
    ((own - reference) * per + per / 2.0).rem_euclid(per) - per / 2.0
}

impl Deck {
    /// (own, reference) bar phases while the decks are synced; the reference is the master (for a follower) or a follower (for the master).
    pub(super) fn phase_meter(&self, beats: &[Beat], pos: f64) -> Option<(f64, f64)> {
        let peer = self.reference()?;
        let pl = peer.loaded.borrow();
        if !pl.ready {
            return None;
        }
        let (ppos, _) = peer.clock(&pl);
        Some((bar_phase(beats, pos)?, bar_phase(&pl.analysis.beats, ppos)?))
    }

    /// SYNC RATE text: the multiple of the master's tempo this deck follows (the tempos are those of the sections under the
    /// two playheads, `pos` is this deck's); "" when it follows nobody.
    fn sync_rate(&self, beats: &[Beat], pos: f64, half_double: bool) -> &'static str {
        let Some(peer) = self.following().then(|| self.master_deck()).flatten() else {
            return "";
        };
        let pl = peer.loaded.borrow();
        tempo_at(beats, pos)
            .zip(tempo_at(&pl.analysis.beats, peer.clock(&pl).0))
            .and_then(|(g, m)| sync::target(g, m * peer.rate(), half_double))
            .map_or("", |(_, k)| rate_text(k))
    }

    pub(super) fn tick(&self) {
        self.poll_decode();
        self.poll_stems();
        self.stemwave_tick();
        self.track_tempo();
        self.follow();
        self.slicer_tick();
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let mut l = self.loaded.borrow_mut();
        if !l.ready {
            return;
        }
        let (pos, playing) = self.clock(&l);
        let opts = OPTS.get();
        // Physical size of each Image element = logical size × scale factor.
        let scale = app.window().scale_factor();
        let osize = (
            (self.overview_size.get().0 * scale).round() as usize,
            (self.overview_size.get().1 * scale).round() as usize,
        );
        let mut dirty = frame::tick_scroll(playing) && self.scroll(&app, &mut l, pos);
        let mut v = self.view.borrow_mut();
        if v.playing != playing {
            let track = l.track.as_ref().map(openfader::plugin::source::track_json);
            let deck = self.index + 1;
            let event = if playing { "deck_play" } else { "deck_pause" };
            openfader::plugin::event::fire(
                event,
                json!({"deck": deck, "track": track, "position_ms": pos}),
            );
            if !playing && at_end(pos, l.duration_ms) {
                v.status = tr!("end of track").into();
                openfader::plugin::event::fire(
                    "track_ended",
                    json!({"deck": deck, "track": track}),
                );
            }
            v.playing = playing;
            dirty = true;
        }
        let lit = self.cue_lit(pos, playing, l.cue_ms);
        if v.at_cue != lit {
            v.at_cue = lit;
            dirty = true;
        }
        let ending = ending(pos, l.duration_ms, playing && l.loop_.is_none());
        if v.ending != ending {
            v.ending = ending;
            dirty = true;
        }
        let armed = self.start_armed();
        if v.sync_armed != armed {
            v.sync_armed = armed;
            dirty = true;
        }
        let cue_angle = jog_angle(f64::from(l.cue_ms));
        let laps = opts
            .countdown
            .then(|| laps_to_hot_cue(&l.analysis.cues, pos))
            .flatten()
            .unwrap_or(-1.0);
        let rate = if opts.sync_rate {
            self.sync_rate(&l.analysis.beats, pos, opts.half_double)
        } else {
            ""
        };
        if (v.jog_cue - cue_angle).abs() > 0.01
            || (v.jog_laps - laps).abs() > 0.003
            || v.jog_countdown != opts.countdown
            || v.sync_rate.as_str() != rate
        {
            (v.jog_cue, v.jog_laps, v.jog_countdown) = (cue_angle, laps, opts.countdown);
            v.sync_rate = rate.into();
            dirty = true;
        }
        let slip = self
            .with_player(|p| p.slip_position())
            .flatten()
            .map(|s| s * 1000.0);
        if osize.0 > 0 && osize.1 > 0 {
            let gpu = render::backend::gpu();
            let x = if gpu {
                0
            } else {
                render::overview::playhead_x(osize.0, pos, l.duration_ms)
            };
            let okey = (
                x,
                osize,
                if gpu {
                    None
                } else {
                    slip.map(|s| render::overview::playhead_x(osize.0, s, l.duration_ms))
                },
                l.cue_ms,
                gpu,
            );
            if l.overview_key != Some(okey) {
                let base = match l.overview_base.take().filter(|c| (c.w, c.h) == osize) {
                    Some(base) => base,
                    None => {
                        eprintln!(
                            "deck {}: overview buffer {}x{} px (element {:?} logical × scale {scale})",
                            self.index,
                            osize.0,
                            osize.1,
                            self.overview_size.get()
                        );
                        render::overview::render(
                            l.overview.as_ref(),
                            &l.analysis,
                            l.duration_ms,
                            osize.0,
                            osize.1,
                        )
                    }
                };
                if gpu {
                    (v.overview, v.overview_head, v.overview_ghost) =
                        render::overview::composed(&base, l.cue_ms, l.duration_ms);
                } else {
                    v.overview =
                        render::overview::with_playhead(&base, pos, l.cue_ms, slip, l.duration_ms);
                }
                l.overview_base = Some(base);
                l.overview_key = Some(okey);
                dirty = true;
            }
        }
        let tenths = pos as u32 / 100;
        if l.time_key != Some(tenths) {
            let (ms, duration) = (pos as u32, l.duration_ms);
            let elapsed = fmt_ms(ms);
            v.time_text = format!("{elapsed} / {}", fmt_ms(duration)).into();
            v.remain_text = format!("-{}", fmt_ms(duration.saturating_sub(ms))).into();
            v.elapsed_text = elapsed.into();
            l.time_key = Some(tenths);
            dirty = true;
        }
        if dirty {
            let snapshot = v.clone();
            drop(v);
            self.model.set_row_data(self.index, snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<Beat> {
        (0..16)
            .map(|i| Beat {
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
                time_ms: i * 500,
            })
            .collect()
    }

    #[test]
    fn bar_phase_follows_the_bar_position() {
        let g = grid();
        assert_eq!(bar_phase(&g, 0.0), Some(0.0));
        assert_eq!(bar_phase(&g, 750.0), Some(0.375)); // half way through beat 2
        assert!((bar_phase(&g, 2250.0).unwrap() - 0.125).abs() < 1e-9); // the next bar
        assert_eq!(bar_phase(&[], 0.0), None);
    }

    #[test]
    fn offset_takes_the_short_way_round_the_bar() {
        assert_eq!(phase_offset(0.5, 0.5, 4.0), 0.0);
        assert!((phase_offset(0.30, 0.25, 4.0) - 0.2).abs() < 1e-9); // 0.2 beat ahead
        assert!((phase_offset(0.05, 0.95, 4.0) - 0.4).abs() < 1e-9); // across the bar line
        assert!((phase_offset(0.95, 0.05, 4.0) + 0.4).abs() < 1e-9);
        assert!((phase_offset(0.75, 0.25, 4.0).abs() - 2.0).abs() < 1e-9); // half a bar apart
        assert!((phase_offset(0.05, 0.95, 3.0) - 0.3).abs() < 1e-9); // a 3-beat bar
    }

    #[test]
    fn bar_phase_wraps_at_the_bar_length() {
        let g: Vec<Beat> = (0..9)
            .map(|i| Beat { bar_pos: (i % 3 + 1) as u16, ..grid()[i as usize] })
            .collect();
        assert!((bar_phase(&g, 1500.0).unwrap()).abs() < 1e-9); // beat 4 = bar 2's first
        assert!((bar_phase(&g, 1000.0).unwrap() - 2.0 / 3.0).abs() < 1e-9);
    }

    fn cue(hot_cue: u16, time_ms: u32) -> Cue {
        Cue {
            hot_cue,
            time_ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    #[test]
    fn the_platter_turns_once_per_1800_ms() {
        assert_eq!(jog_angle(0.0), 0.0);
        assert!((jog_angle(450.0) - 90.0).abs() < 1e-4);
        assert!((jog_angle(1800.0 + 900.0) - 180.0).abs() < 1e-4);
        assert!(
            jog_angle(-450.0) > 269.0,
            "before the start the platter is behind the top"
        );
    }

    #[test]
    fn countdown_looks_at_the_next_hot_cue_within_five_laps() {
        let cues = [cue(0, 1000), cue(2, 5000), cue(1, 20_000)];
        // the memory cue (hot_cue 0) is not a hot cue; 5000 ms ahead of 3200 is 1800 ms = 1 lap
        assert_eq!(laps_to_hot_cue(&cues, 3200.0), Some(1.0));
        // the hot cue behind the playhead does not count; the one 9000 ms ahead is exactly 5 laps
        assert_eq!(laps_to_hot_cue(&cues, 11_000.0), Some(5.0));
        assert_eq!(
            laps_to_hot_cue(&cues, 10_000.0),
            None,
            "more than 5 laps away"
        );
        assert_eq!(laps_to_hot_cue(&cues, 25_000.0), None, "nothing ahead");
        assert_eq!(laps_to_hot_cue(&[], 0.0), None);
    }
}
