//! Automix planner: pure functions from two tracks' analysis to a transition. No audio, no UI.
//!
//! Bars are 4 beats (ponytail: 4/4 only) numbered from the grid's first downbeat (`bar_pos == 1`).
//! - Mix-out point of the outgoing track: the start of its last `Outro` phrase (rounded to a bar) when
//!   Analysis phrases exist, else `bars` bars before its last whole bar. If the track is already past that,
//!   or [`plan`] is asked to mix `asap`, the next bar line after [`LEAD_MS`].
//! - Mix-in point of the incoming track: its first phrase start when phrases exist, else its first
//!   downbeat. It is started so that point meets the mix-out point.
//! - The fade lasts `bars` bars of the outgoing grid, fewer when the track ends first.
//!
//! The rules are our own design.

use crate::app::deck::{beat_at, grid_bpm, ms_at};
use openfader::analysis::{Analysis, Beat, PhraseKind};

/// Track time kept between "now" and the earliest mix-out point (the incoming deck needs a moment to start).
const LEAD_MS: f64 = 150.0;
const BEATS_PER_BAR: f64 = 4.0;
/// Measured offsets beyond this are not an output-latency error but a different problem; leave them.
const MAX_ALIGN_MS: f64 = 500.0;
/// Offsets below this are within the measuring noise.
const MIN_ALIGN_MS: f64 = 2.0;

/// One transition. Times are track milliseconds on each track's own timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Outgoing track position where the incoming track starts and the fade begins.
    pub out_ms: f64,
    /// Incoming track position that plays at `out_ms`.
    pub in_ms: f64,
    /// Fade length in bars of the outgoing grid (≤ the requested count).
    pub bars: u32,
    /// Fade length on the outgoing track's timeline (so wall time = this / outgoing rate).
    pub fade_ms: f64,
}

/// Fade state at progress `x` (0 = fade start, 1 = done).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mix {
    /// Crossfader travel from the outgoing deck (0) to the incoming one (1).
    pub travel: f32,
    /// Bass swap: the incoming LOW band stays cut until halfway, then the outgoing one is cut.
    pub low_cut_in: bool,
    pub low_cut_out: bool,
}

pub fn mix_at(x: f64) -> Mix {
    let x = x.clamp(0.0, 1.0);
    Mix {
        travel: x as f32,
        low_cut_in: x < 0.5,
        low_cut_out: x >= 0.5,
    }
}

/// Beat number (fractional index) of the first downbeat.
fn first_downbeat(beats: &[Beat]) -> f64 {
    beats.iter().position(|b| b.bar_pos == 1).unwrap_or(0) as f64
}

/// Where the incoming track enters: first phrase start, else first downbeat.
fn anchor(a: &Analysis) -> Option<f64> {
    match a.phrases.as_ref().and_then(|p| p.list.first()) {
        Some(p) => Some(p.span_ms(&a.beats).0),
        None => ms_at(&a.beats, first_downbeat(&a.beats)),
    }
}

/// Bar index (from the first downbeat `d0`) of the last `Outro` phrase's start.
fn outro_bar(a: &Analysis, d0: f64) -> Option<f64> {
    let p = a
        .phrases
        .as_ref()?
        .list
        .iter()
        .rev()
        .find(|p| p.kind == PhraseKind::Outro)?;
    Some(((beat_at(&a.beats, p.span_ms(&a.beats).0)? - d0) / BEATS_PER_BAR).round())
}

/// Plans a mix of `inc` into `out` (`out_dur` ms long). `from_ms` is where `out` plays now; the mix
/// never starts earlier than [`LEAD_MS`] after it. `asap` ignores the ideal point (SKIP). `None`:
/// a grid is missing, or less than one whole bar is left (the driver cuts instead).
pub fn plan(
    out: &Analysis,
    out_dur: u32,
    inc: &Analysis,
    bars: u32,
    from_ms: f64,
    asap: bool,
) -> Option<Plan> {
    let ob = &out.beats;
    grid_bpm(ob)?;
    grid_bpm(&inc.beats)?;
    let d0 = first_downbeat(ob);
    let bar_of = |ms: f64| beat_at(ob, ms).map(|b| (b - d0) / BEATS_PER_BAR);
    let end_bar = bar_of(out_dur as f64)?.floor();
    let first_bar = bar_of(0.0)?.ceil(); // bars before the first beat's time are not on the track
    let earliest = bar_of(from_ms + LEAD_MS)?.ceil();
    let ideal = outro_bar(out, d0)
        .filter(|k| *k < end_bar)
        .unwrap_or(end_bar - bars as f64);
    let k = if asap { earliest } else { ideal.max(earliest) }.max(first_bar);
    let left = end_bar - k;
    if left < 1.0 {
        return None;
    }
    let bars = (bars as f64).min(left) as u32;
    let out_ms = ms_at(ob, d0 + BEATS_PER_BAR * k)?;
    let fade_ms = ms_at(ob, d0 + BEATS_PER_BAR * (k + bars as f64))? - out_ms;
    Some(Plan {
        out_ms,
        in_ms: anchor(inc)?,
        bars,
        fade_ms,
    })
}

/// Playback rate that makes `inc` run at `out`'s effective tempo (`out` at `rate_out`); half or double
/// time when that is closer to 1. `None` without a grid or outside 0.5 to 2×.
pub fn rate_for(out: &[Beat], rate_out: f64, inc: &[Beat]) -> Option<f64> {
    let r = grid_bpm(out)? * rate_out / grid_bpm(inc)?;
    [r, r / 2.0, r * 2.0]
        .into_iter()
        .filter(|r| (0.5..=2.0).contains(r))
        .min_by(|a, b| (a - 1.0).abs().total_cmp(&(b - 1.0).abs()))
}

/// `rate` moved towards the deck's own tempo (1.0) by at most `step`, landing exactly on it.
pub fn glide(rate: f64, step: f64) -> f64 {
    let d = rate - 1.0;
    if d.abs() <= step {
        1.0
    } else {
        rate - d.signum() * step
    }
}

/// Where the incoming deck should be at the outgoing audible position `out_pos`, both rates known.
pub fn expected_in(p: &Plan, out_pos: f64, rate_out: f64, rate_in: f64) -> f64 {
    p.in_ms + (out_pos - p.out_ms) / rate_out * rate_in
}

/// One-shot beat alignment of the incoming deck once it is audible. It was started on the outgoing
/// deck's audible position, but a started deck is heard one output latency (plus buffer) later, so it
/// lags by `-e`. Both decks' *written* positions are `latency` ahead of their audible ones, so the seek
/// target is `audible - 2e`. `None` when the error is below the measuring noise or implausibly large.
// ponytail: ignores the wait until the next audio buffer applies the seek (≤ one buffer, about 10 ms).
pub fn align_target(in_pos: f64, expected: f64) -> Option<u32> {
    let e = in_pos - expected;
    (MIN_ALIGN_MS..MAX_ALIGN_MS)
        .contains(&e.abs())
        .then(|| (in_pos - 2.0 * e).max(0.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::{Mood, Phrase, Phrases};

    /// 120 BPM (500 ms beats), 4/4, first beat at `start` ms, `n` beats, first beat on bar position `first`.
    fn analysis(start: u32, n: u32, first: u16) -> Analysis {
        let beats = (0..n)
            .map(|i| Beat {
                bar_pos: ((i as u16 + first - 1) % 4) + 1,
                bpm_x100: 12000,
                time_ms: start + i * 500,
            })
            .collect();
        Analysis {
            beats,
            ..Default::default()
        }
    }

    fn with_phrases(mut a: Analysis, list: Vec<(u32, u32, PhraseKind)>) -> Analysis {
        let list = list
            .into_iter()
            .map(|(beat, end_beat, kind)| Phrase {
                beat,
                end_beat,
                kind,
                variant: 0,
                fill: None,
            })
            .collect();
        a.phrases = Some(Phrases {
            mood: Mood::High,
            list,
        });
        a
    }

    // 5 min at 120 BPM: 600 beats, 150 bars, 1 bar = 2000 ms.
    const DUR: u32 = 300_000;

    #[test]
    fn glide_returns_to_the_own_tempo_from_either_side_without_overshoot() {
        for start in [1.08, 0.93] {
            let (mut r, mut n) = (start, 0);
            while r != 1.0 {
                let next = glide(r, 0.0005);
                assert!((next - 1.0).abs() < (r - 1.0).abs() && (next - 1.0) * (r - 1.0) >= 0.0);
                r = next;
                n += 1;
            }
            assert!(n <= (start - 1.0_f64).abs().div_euclid(0.0005) as i32 + 2);
        }
        assert_eq!(glide(1.0, 0.0005), 1.0);
    }

    #[test]
    fn mix_out_is_n_bars_before_the_last_whole_bar_and_the_next_track_enters_on_its_downbeat() {
        let (a, b) = (analysis(100, 600, 1), analysis(240, 600, 3)); // b's first downbeat is its 3rd beat
        let p = plan(&a, DUR, &b, 16, 0.0, false).unwrap();
        // last whole bar line ≤ 300000 ms: bar 149 (beat 596 → 100 + 596*500 = 298100); 16 bars earlier = 266100
        assert_eq!(p.out_ms, 100.0 + 4.0 * 133.0 * 500.0);
        assert_eq!((p.bars, p.fade_ms), (16, 32_000.0));
        assert_eq!(p.in_ms, 240.0 + 2.0 * 500.0); // b: beats 0,1 are bar positions 3,4; downbeat = beat 2
    }

    #[test]
    fn bar_count_and_phrase_anchor_change_the_plan() {
        let (a, b) = (analysis(0, 600, 1), analysis(0, 600, 1));
        assert_eq!(
            plan(&a, DUR, &b, 8, 0.0, false).unwrap().out_ms,
            300_000.0 - 8.0 * 2000.0
        );
        // phrases: first phrase of b starts at beat 9 (1-based) = 4000 ms; a's last Outro starts at beat 561 = 280000 ms
        let a = with_phrases(
            a,
            vec![(1, 561, PhraseKind::Verse), (561, 601, PhraseKind::Outro)],
        );
        let b = with_phrases(
            b,
            vec![(9, 41, PhraseKind::Intro), (41, 100, PhraseKind::Verse)],
        );
        let p = plan(&a, DUR, &b, 16, 0.0, false).unwrap();
        assert_eq!((p.out_ms, p.in_ms), (280_000.0, 4_000.0));
        assert_eq!((p.bars, p.fade_ms), (10, 20_000.0)); // 10 bars left after the Outro start: the fade shrinks
    }

    #[test]
    fn a_passed_mix_point_slides_to_the_next_bar_and_asap_does_too() {
        let (a, b) = (analysis(0, 600, 1), analysis(0, 600, 1));
        let ideal = plan(&a, DUR, &b, 16, 0.0, false).unwrap().out_ms;
        assert_eq!(ideal, 268_000.0);
        // already at 270 s: next bar line after 270150 ms is 272000
        assert_eq!(
            plan(&a, DUR, &b, 16, 270_000.0, false).unwrap().out_ms,
            272_000.0
        );
        // SKIP at 10 s: mix at the next bar line, with the full fade
        let p = plan(&a, DUR, &b, 16, 10_000.0, true).unwrap();
        assert_eq!((p.out_ms, p.bars), (12_000.0, 16));
        // less than one bar left: nothing to fade
        assert!(plan(&a, DUR, &b, 16, 298_500.0, false).is_none());
    }

    #[test]
    fn short_tracks_clamp_to_the_first_bar_and_missing_grids_have_no_plan() {
        let (a, b) = (analysis(0, 40, 1), analysis(0, 40, 1)); // 20 s, 10 bars
        let p = plan(&a, 20_000, &b, 32, 0.0, false).unwrap();
        // 20 s, 10 bars: the mix may not start before one lead after "now", so bar 1; 9 bars are left for the fade
        assert_eq!((p.out_ms, p.bars), (2_000.0, 9));
        assert!(plan(&Analysis::default(), DUR, &b, 8, 0.0, false).is_none());
        assert!(plan(&a, 20_000, &Analysis::default(), 8, 0.0, false).is_none());
    }

    #[test]
    fn tempo_matches_with_half_or_double_time_and_the_player_range() {
        let g = |bpm_x100: u16| {
            vec![
                Beat {
                    bar_pos: 1,
                    bpm_x100,
                    time_ms: 0,
                },
                Beat {
                    bar_pos: 2,
                    bpm_x100,
                    time_ms: 400,
                },
            ]
        };
        assert!((rate_for(&g(12800), 1.0, &g(12000)).unwrap() - 128.0 / 120.0).abs() < 1e-9);
        assert!((rate_for(&g(12800), 1.04, &g(12800)).unwrap() - 1.04).abs() < 1e-9); // follows the outgoing deck's tempo fader
        assert!((rate_for(&g(17000), 1.0, &g(8500)).unwrap() - 1.0).abs() < 1e-9); // 170 vs 85 BPM: run in double time
        assert!(rate_for(&g(12800), 1.0, &[]).is_none() && rate_for(&[], 1.0, &g(12800)).is_none());
    }

    #[test]
    fn fade_runs_the_crossfader_across_and_swaps_the_bass_at_the_middle() {
        assert_eq!(
            mix_at(0.0),
            Mix {
                travel: 0.0,
                low_cut_in: true,
                low_cut_out: false
            }
        );
        assert_eq!(
            mix_at(0.49),
            Mix {
                travel: 0.49,
                low_cut_in: true,
                low_cut_out: false
            }
        );
        assert_eq!(
            mix_at(0.5),
            Mix {
                travel: 0.5,
                low_cut_in: false,
                low_cut_out: true
            }
        );
        assert_eq!(mix_at(1.0).travel, 1.0);
        assert_eq!((mix_at(-1.0).travel, mix_at(2.0).travel), (0.0, 1.0));
        // never both cut, never neither (bass is always carried by exactly one deck)
        for i in 0..=100 {
            let m = mix_at(i as f64 / 100.0);
            assert!(m.low_cut_in != m.low_cut_out);
        }
    }

    #[test]
    fn incoming_position_follows_both_rates_and_alignment_cancels_the_output_latency() {
        let p = Plan {
            out_ms: 1000.0,
            in_ms: 500.0,
            bars: 8,
            fade_ms: 16_000.0,
        };
        assert_eq!(expected_in(&p, 1000.0, 1.0, 1.0), 500.0);
        assert_eq!(expected_in(&p, 1500.0, 1.0, 1.06), 500.0 + 500.0 * 1.06);
        assert_eq!(expected_in(&p, 1500.0, 2.0, 1.0), 500.0 + 250.0); // outgoing at 2×: half the wall time
        // incoming deck heard 40 ms late: audible 500 where 540 is expected, so its written position is 540 and should be 580
        assert_eq!(align_target(500.0, 540.0), Some(580));
        assert_eq!(align_target(540.0, 540.5), None); // inside the noise
        assert_eq!(align_target(500.0, 5000.0), None); // implausible: not latency
        assert_eq!(align_target(0.0, 10.0), Some(20));
    }
}
