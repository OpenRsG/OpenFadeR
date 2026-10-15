//! Metronome (Preferences > Audio > Metronome: tone and volume, used while monitoring beat grids).
//!
//! A click is mixed into a deck's master path on every beat of that deck's grid while the deck's metronome is on
//! (`Player::set_metronome`, `Player::set_beats`). The click lands on the master mix (the speakers, and the
//! recording), not on the headphone feed, because the headphone feed is silent on a single stereo device without
//! SPLIT (our choice). The three timbres are rendered once when the stream
//! opens ([`Clicks`]); the callback only reads them, so it never allocates.
//!
//! ponytail: no accent on the bar's first beat (the grid handed over is a plain list of beat times); one click
//! voice per deck, so a click is cut by the next beat if the tempo exceeds ~1/click-length. Loop wraps inside a
//! buffer are not followed (beats past the loop end in that buffer may click early).

use super::Loop;

/// Number of timbres.
pub const TONES: usize = 3;
/// Most beats one buffer can schedule (a buffer is far shorter than the spacing between beats).
const MAX_HITS: usize = 8;

/// `(frequency Hz, second partial ratio, length s)` of each timbre: a sine beep, a wood-block tick, a low thump.
const TIMBRES: [(f32, f32, f32); TONES] =
    [(1000.0, 0.0, 0.03), (1700.0, 2.6, 0.02), (600.0, 0.0, 0.06)];
/// Click peak before the volume knob.
const PEAK: f32 = 0.8;

/// Frame offsets (inside one buffer) at which a beat falls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hits {
    at: [usize; MAX_HITS],
    n: usize,
}

impl Hits {
    pub const NONE: Hits = Hits {
        at: [0; MAX_HITS],
        n: 0,
    };

    pub fn as_slice(&self) -> &[usize] {
        &self.at[..self.n]
    }
}

/// The beats (`beats` = beat times in track ms, ascending) that play inside a buffer of `n` frames: the buffer
/// starts at source frame `start` and moves `fps` source frames per second at the device rate `dev_rate`
/// (`src_rate` = the track's rate). Nothing while stopped or running backwards; a loop end cuts the span.
pub fn schedule(
    beats: &[u32],
    src_rate: f64,
    start: f64,
    fps: f64,
    dev_rate: f64,
    n: usize,
    lp: Loop,
) -> Hits {
    let step = fps / dev_rate;
    if !(step > 0.0) || n == 0 || !(src_rate > 0.0) {
        return Hits::NONE;
    }
    let mut end = start + step * n as f64;
    if let Some((_, le)) = lp {
        if start < le && le < end {
            end = le;
        }
    }
    let frame = |ms: u32| ms as f64 * src_rate / 1000.0;
    let first = beats.partition_point(|&b| frame(b) < start);
    let mut h = Hits::NONE;
    for &b in &beats[first..] {
        let f = frame(b);
        if f >= end || h.n == MAX_HITS {
            break;
        }
        h.at[h.n] = (((f - start) / step) as usize).min(n - 1);
        h.n += 1;
    }
    h
}

/// The pre-rendered clicks of every timbre at one sample rate.
pub struct Clicks {
    tones: [Vec<f32>; TONES],
}

impl Clicks {
    pub fn new(rate: u32) -> Clicks {
        Clicks {
            tones: TIMBRES.map(|(hz, partial, secs)| {
                let tau = secs / 5.0; // -43 dB at the end of the click
                (0..(secs * rate as f32) as usize)
                    .map(|i| {
                        let t = i as f32 / rate as f32;
                        let tau_f = std::f32::consts::TAU * t;
                        let tone = (tau_f * hz).sin()
                            + if partial > 0.0 {
                                0.5 * (tau_f * hz * partial).sin()
                            } else {
                                0.0
                            };
                        PEAK * tone * (-t / tau).exp() / if partial > 0.0 { 1.5 } else { 1.0 }
                    })
                    .collect()
            }),
        }
    }
}

/// One deck's click playback (audio thread).
#[derive(Default)]
pub struct Voice {
    tone: usize,
    pos: usize,
    live: bool,
}

impl Voice {
    /// Adds the clicks of `hits` (and the tail of an earlier one) to the master path `.0` of `out`, at `volume` (0..=1).
    /// `tone` is the timbre used for clicks starting in this buffer.
    pub fn mix(
        &mut self,
        clicks: &Clicks,
        tone: usize,
        volume: f32,
        hits: &Hits,
        out: &mut [([f32; 2], [f32; 2])],
    ) {
        if !self.live && hits.n == 0 {
            return;
        }
        let mut next = 0;
        for (i, o) in out.iter_mut().enumerate() {
            while next < hits.n && hits.at[next] <= i {
                (self.tone, self.pos, self.live) = (tone.min(TONES - 1), 0, true);
                next += 1;
            }
            if !self.live {
                continue;
            }
            if let Some(&c) = clicks.tones[self.tone].get(self.pos) {
                let x = c * volume;
                o.0[0] += x;
                o.0[1] += x;
                self.pos += 1;
            } else {
                self.live = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 bpm grid: a beat every 500 ms, the first at 100 ms. Track at 44.1 kHz played at 1x on a 48 kHz device.
    const BEATS: [u32; 5] = [100, 600, 1100, 1600, 2100];

    fn run(start: f64, n: usize, lp: Loop) -> Hits {
        schedule(&BEATS, 44_100.0, start, 44_100.0, 48_000.0, n, lp)
    }

    #[test]
    fn a_beat_inside_the_buffer_is_scheduled_at_its_frame() {
        // 100 ms = source frame 4410; the buffer starts at 4000 and advances 0.91875 source frames per output frame.
        let h = run(4000.0, 1024, None);
        assert_eq!(
            h.as_slice(),
            [((4410.0 - 4000.0) / (44_100.0 / 48_000.0)) as usize]
        );
    }

    #[test]
    fn consecutive_buffers_neither_miss_nor_repeat_a_beat() {
        let (mut pos, mut got) = (0.0, 0);
        for _ in 0..200 {
            got += run(pos, 256, None).as_slice().len();
            pos += 256.0 * 44_100.0 / 48_000.0;
        }
        // 200 * 256 frames = 1.07 s: beats at 100 and 600 ms
        assert_eq!(got, 2);
        // a beat exactly on a buffer boundary belongs to the buffer that starts there
        let at = 4410.0;
        assert_eq!(run(at, 16, None).as_slice(), [0]);
        assert!(run(at - 15.0, 16, None).as_slice().is_empty());
    }

    #[test]
    fn nothing_while_stopped_backwards_or_past_the_loop_end() {
        assert!(
            schedule(&BEATS, 44_100.0, 4000.0, 0.0, 48_000.0, 1024, None)
                .as_slice()
                .is_empty()
        );
        assert!(
            schedule(&BEATS, 44_100.0, 4000.0, -44_100.0, 48_000.0, 1024, None)
                .as_slice()
                .is_empty()
        );
        assert!(
            run(4000.0, 1024, Some((0.0, 4200.0))).as_slice().is_empty(),
            "the loop wraps before the beat"
        );
        assert_eq!(run(4000.0, 1024, Some((0.0, 9000.0))).as_slice().len(), 1);
        assert!(
            schedule(&[], 44_100.0, 0.0, 44_100.0, 48_000.0, 64, None)
                .as_slice()
                .is_empty()
        );
    }

    #[test]
    fn clicks_decay_and_differ_per_timbre() {
        let c = Clicks::new(48_000);
        for t in 0..TONES {
            let x = &c.tones[t];
            let (head, tail) = (
                x[..x.len() / 4].iter().fold(0f32, |m, v| m.max(v.abs())),
                x[x.len() * 3 / 4..]
                    .iter()
                    .fold(0f32, |m, v| m.max(v.abs())),
            );
            assert!(
                head > 0.2 && head <= PEAK + 1e-6 && tail < head / 5.0,
                "tone {t}: head {head} tail {tail}"
            );
        }
        assert_ne!(c.tones[0], c.tones[1]);
        assert_ne!(c.tones[0].len(), c.tones[2].len());
    }

    #[test]
    fn the_voice_starts_at_the_hit_scales_by_volume_and_rings_into_the_next_buffer() {
        let c = Clicks::new(48_000);
        let mut v = Voice::default();
        let mut out = vec![([0.0f32; 2], [0.0f32; 2]); 100];
        let hits = Hits {
            at: [10, 0, 0, 0, 0, 0, 0, 0],
            n: 1,
        };
        v.mix(&c, 0, 0.5, &hits, &mut out);
        assert!(
            out[..10].iter().all(|o| o.0 == [0.0; 2]),
            "silent before the beat"
        );
        assert_eq!(out[10].0, [0.0; 2], "a sine starts at zero");
        assert_eq!(out[11].0[0], c.tones[0][1] * 0.5);
        assert_eq!(out[11].0[0], out[11].0[1]);
        assert!(
            out.iter().all(|o| o.1 == [0.0; 2]),
            "the cue path is untouched"
        );
        let mut next = vec![([0.0f32; 2], [0.0f32; 2]); 2000];
        v.mix(&c, 0, 0.5, &Hits::NONE, &mut next);
        assert_eq!(
            next[0].0[0],
            c.tones[0][90] * 0.5,
            "the click carries on in the next buffer"
        );
        assert!(
            next[1500..].iter().all(|o| o.0 == [0.0; 2]) && !v.live,
            "and ends (30 ms = 1440 frames)"
        );
    }
}
