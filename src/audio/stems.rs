//! STEMS on the audio side: the separated parts ([`Stems`]), the frame source the voice and the stretcher read from
//! ([`Source`]) and the track with per-stem gains applied ([`Mixed`]).
//!
//! The stems share the track's sample rate and frame count, so one read position addresses the mix and every stem: key lock,
//! scratch, loops, reverse and slip keep working unchanged. Gains are applied while reading (the voice ramps them per output
//! frame, the stretcher takes them per grain, which its Hann crossfade smooths), so mute/solo never clicks.
//!
//! Mixing keeps the separator's residual (`mix - Σ stems`, the part the model did not assign) in proportion to the mean gain:
//! `out = mix·ρ + Σ stem_k·(g_k - ρ)` with `ρ = mean(g)`. All stems at 1 gives the original track bit for bit, one stem alone
//! gives that stem plus a quarter of the residual, and the output is continuous in every gain.
//!
//! The separation fills [`Stems`] while it is played (the track before STEMS analysis is completed plays as is):
//! the track is cut into overlapping segments of a [`Grid`], each separated on its own and
//! cross-faded with its neighbours ([`Stems::deposit`]); a frame is [`ready`](Stems::ready) once every segment that touches it is
//! in. [`Mixed`] plays the original track where it is not.

use super::{Pcm, Track16};
use parking_lot::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicI16,
    Ordering::{Acquire, Relaxed, Release},
};

/// Vocal, instruments, bass, drums: the 4-stem set (`3-stem` mode merges bass into instruments at the gains).
pub const COUNT: usize = 4;
pub const NAMES: [&str; COUNT] = ["VOCAL", "INST", "BASS", "DRUMS"];
pub const VOCAL: usize = 0;
pub const INST: usize = 1;
pub const BASS: usize = 2;
pub const DRUMS: usize = 3;

/// How a track is cut for separation: segment `s` covers frames `s·stride .. s·stride + len` (the last one up to the end), and
/// shares its first `overlap` frames with segment `s-1`. Needs `overlap < len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub len: usize,
    pub overlap: usize,
}

impl Grid {
    pub fn stride(self) -> usize {
        self.len - self.overlap
    }

    /// Segments of a track of `frames` frames.
    pub fn count(self, frames: usize) -> usize {
        1 + frames.saturating_sub(self.len).div_ceil(self.stride())
    }

    pub fn start(self, s: usize) -> usize {
        s * self.stride()
    }

    /// The segment whose first frame is nearest at or before `frame`, of `count`.
    pub fn segment_of(self, frame: usize, count: usize) -> usize {
        (frame / self.stride()).min(count - 1)
    }
}

/// Blend weight of frame `i` of a segment: ramps up over the overlap unless `first`, down over the last one unless `last`.
/// The weights of two neighbours sum to 1 in every overlap frame.
fn weight(i: usize, g: Grid, first: bool, last: bool) -> f32 {
    let ramp = (g.overlap + 1) as f32;
    let up = if first || i >= g.overlap {
        1.0
    } else {
        (i + 1) as f32 / ramp
    };
    let down = if last || i < g.stride() {
        1.0
    } else {
        (g.len - i) as f32 / ramp
    };
    up * down
}

fn to_i16(v: f32) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

/// `Vec<i16>` as `Vec<AtomicI16>`, no copy.
fn atomic(v: Vec<i16>) -> Box<[AtomicI16]> {
    let mut v = std::mem::ManuallyDrop::new(v);
    let (p, len, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
    // SAFETY: `AtomicI16` has the same size, alignment and bit validity as `i16` (std docs), and the buffer is taken over whole.
    unsafe { Vec::from_raw_parts(p.cast::<AtomicI16>(), len, cap) }.into_boxed_slice()
}

/// Interleaved stereo i16 per stem (index = [`VOCAL`]…[`DRUMS`]) at the track's rate and length, filled segment by segment
/// while being read ([`Stems::ready`] says where it may be).
pub struct Stems {
    data: [Box<[AtomicI16]>; COUNT],
    frames: usize,
    grid: Grid,
    /// Segment `s` is separated and blended in.
    done: Box<[AtomicBool]>,
    /// Seam `s` (frames shared by segment `s-1` and `s`): one side has deposited its weighted share.
    seams: Box<[Mutex<bool>]>,
}

impl Stems {
    /// Silent stems of `frames` frames, nothing ready.
    pub fn new(frames: usize, grid: Grid) -> Stems {
        assert!(
            grid.len > 0 && grid.overlap < grid.len,
            "bad stems grid {grid:?}"
        );
        let n = grid.count(frames);
        Stems {
            data: std::array::from_fn(|_| atomic(vec![0; 2 * frames])),
            frames,
            grid,
            done: (0..n).map(|_| AtomicBool::new(false)).collect(),
            seams: (0..n).map(|_| Mutex::new(false)).collect(),
        }
    }

    /// Finished stems (a cache hit, a test fixture): `data[k]` is stem `k`, interleaved stereo, all the same length.
    pub fn complete(data: [Vec<i16>; COUNT]) -> Stems {
        let frames = data[0].len() / 2;
        assert!(
            data.iter().all(|d| d.len() == 2 * frames),
            "stems differ in length"
        );
        Stems {
            data: data.map(atomic),
            frames,
            grid: Grid {
                len: frames.max(1),
                overlap: 0,
            },
            done: Box::new([AtomicBool::new(true)]),
            seams: Box::new([Mutex::new(false)]),
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    pub fn segments(&self) -> usize {
        self.done.len()
    }

    /// Segments that are in.
    pub fn segments_done(&self) -> usize {
        self.done.iter().filter(|d| d.load(Acquire)).count()
    }

    pub fn segment_done(&self, s: usize) -> bool {
        self.done.get(s).is_some_and(|d| d.load(Acquire))
    }

    pub fn is_complete(&self) -> bool {
        self.done.iter().all(|d| d.load(Acquire))
    }

    /// Frame `i` is final: every segment that touches it is in.
    #[inline]
    pub fn ready(&self, i: usize) -> bool {
        let g = self.grid;
        let s = g.segment_of(i, self.done.len());
        self.done[s].load(Acquire)
            && (s == 0 || i - g.start(s) >= g.overlap || self.done[s - 1].load(Acquire))
    }

    /// Frame `i` of stem `k` (`i < frames`).
    #[inline]
    fn at(&self, k: usize, i: usize) -> [f32; 2] {
        let d = &self.data[k];
        [
            d[2 * i].load(Relaxed) as f32 * (1.0 / 32768.0),
            d[2 * i + 1].load(Relaxed) as f32 * (1.0 / 32768.0),
        ]
    }

    /// The frames `(i, j, t)` that `pos` lies between (`j` is `i + 1` when that is ready, else `i`) and the blend `t`; `None`
    /// past the end or where frame `i` is not ready.
    #[inline]
    fn around(&self, pos: f64) -> Option<(usize, usize, f32)> {
        let i = pos as usize;
        if i >= self.frames || !self.ready(i) {
            return None;
        }
        let j = (i + 1).min(self.frames - 1);
        Some((
            i,
            if self.ready(j) { j } else { i },
            (pos - i as f64) as f32,
        ))
    }

    /// Stem `k` at a point found by [`Stems::around`], linearly interpolated.
    #[inline]
    fn lerp(&self, k: usize, (i, j, t): (usize, usize, f32)) -> [f32; 2] {
        let (a, b) = (self.at(k, i), self.at(k, j));
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    /// Copies `out.len() / 2` frames of stem `k` from frame `from` (interleaved stereo), ready or not.
    pub fn read(&self, k: usize, from: usize, out: &mut [i16]) {
        for (o, a) in out.iter_mut().zip(&self.data[k][2 * from..]) {
            *o = a.load(Relaxed);
        }
    }

    /// Stores interleaved stereo `v` into stem `k` from frame `from`; says nothing about readiness (see [`Stems::finish`]).
    pub fn write(&self, k: usize, from: usize, v: &[i16]) {
        for (a, &x) in self.data[k][2 * from..].iter().zip(v) {
            a.store(x, Relaxed);
        }
    }

    /// Everything is written (a cache hit read in with [`Stems::write`]): all segments are in.
    pub fn finish(&self) {
        self.done.iter().for_each(|d| d.store(true, Release));
    }

    /// The stems of segment `s` (`parts[k]`: stem `k`, interleaved stereo f32, `grid.len` frames; fewer for the last segment),
    /// blended with the weights of the [`Grid`]: the head and tail overlaps are added to what the neighbour left there, in
    /// any order, from any thread. A frame is only [`ready`](Stems::ready) once both sides are in.
    pub fn deposit(&self, s: usize, parts: [&[f32]; COUNT]) {
        self.deposit_frames(s, parts[0].len() / 2, |i| {
            std::array::from_fn(|k| [parts[k][2 * i], parts[k][2 * i + 1]])
        });
    }

    /// Deposits segment frames without an interleaved intermediate buffer.
    pub fn deposit_frames(
        &self,
        s: usize,
        frames: usize,
        frame: impl Fn(usize) -> [[f32; 2]; COUNT],
    ) {
        let g = self.grid;
        let (first, last) = (s == 0, s + 1 == self.done.len());
        let start = g.start(s);
        let valid = frames.min(self.frames - start);
        let (head, tail) = (
            if first { 0 } else { g.overlap.min(valid) },
            if last { valid } else { g.stride().min(valid) },
        );
        let put = |range: std::ops::Range<usize>, add: bool| {
            for i in range {
                let w = weight(i, g, first, last) * 32768.0;
                let parts = frame(i);
                for k in 0..COUNT {
                    for c in 0..2 {
                        let cell = &self.data[k][2 * (start + i) + c];
                        let v = parts[k][c] * w + if add { cell.load(Relaxed) as f32 } else { 0.0 };
                        cell.store(to_i16(v), Relaxed);
                    }
                }
            }
        };
        if head > 0 {
            let mut other = self.seams[s].lock();
            put(0..head, *other);
            *other = true;
        }
        put(head..tail, false);
        if tail < valid {
            let mut other = self.seams[s + 1].lock();
            put(tail..valid, *other);
            *other = true;
        }
        self.done[s].store(true, Release);
    }
}

/// A stereo signal readable at fractional source-frame positions (windowed-sinc interpolation, `None` past the end).
pub trait Source {
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]>;

    /// [`Source::frame_at`] for a read that advances `step` source frames per output frame: above 1 the interpolation
    /// low-passes to the new Nyquist. The default ignores the step (a source built from several reads takes it from the
    /// one that matters: [`Mixed`]).
    #[inline]
    fn frame_at_step(&self, pos: f64, _step: f64) -> Option<[f32; 2]> {
        self.frame_at(pos)
    }
}

impl Source for Pcm {
    #[inline]
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        Pcm::frame_at(self, pos)
    }
    #[inline]
    fn frame_at_step(&self, pos: f64, step: f64) -> Option<[f32; 2]> {
        Pcm::frame_at_step(self, pos, step)
    }
}

/// The track `pcm` with the stem gains `gain` (0..=1 each) applied; see the module doc for the formula. Where the stems are
/// not ready yet it is the track itself.
pub struct Mixed<'a> {
    pub pcm: &'a Track16,
    pub stems: &'a Stems,
    pub gain: [f32; COUNT],
}

impl Source for Mixed<'_> {
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        self.frame_at_step(pos, 1.0)
    }

    fn frame_at_step(&self, pos: f64, step: f64) -> Option<[f32; 2]> {
        let mut out = self.pcm.frame_at_step(pos, step)?;
        let Some(at) = self.stems.around(pos) else {
            return Some(out);
        };
        let rho = self.gain.iter().sum::<f32>() / COUNT as f32;
        out = out.map(|v| v * rho);
        for k in 0..COUNT {
            let w = self.gain[k] - rho;
            if w != 0.0 {
                let s = self.stems.lerp(k, at);
                for c in 0..2 {
                    out[c] += w * s[c];
                }
            }
        }
        Some(out)
    }
}

/// `Σ_{k in mask} gain[k]·stem_k` (bit k of `mask` = stem k): the part a stem FX unit works on. Zeros past the end and where
/// the stems are not ready.
pub struct Part<'a> {
    pub stems: &'a Stems,
    pub gain: [f32; COUNT],
    pub mask: u8,
}

impl Source for Part<'_> {
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        let mut out = [0.0; 2];
        let Some(at) = self.stems.around(pos).filter(|_| pos >= 0.0) else {
            return Some(out);
        };
        for k in (0..COUNT).filter(|k| self.mask >> k & 1 != 0 && self.gain[*k] != 0.0) {
            let s = self.stems.lerp(k, at);
            for c in 0..2 {
                out[c] += self.gain[k] * s[c];
            }
        }
        Some(out)
    }
}

/// The mix without the part: `mixed − part`; ends where the track ends.
pub struct Rest<'a> {
    pub mixed: Mixed<'a>,
    pub part: Part<'a>,
}

impl Source for Rest<'_> {
    fn frame_at(&self, pos: f64) -> Option<[f32; 2]> {
        let m = self.mixed.frame_at(pos)?;
        let p = self.part.frame_at(pos)?;
        Some([m[0] - p[0], m[1] - p[1]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4-frame mix 1.0 and stems of constant 0.1/0.2/0.3/0.4 (sum 1.0 - residual 0).
    fn fixture(mix: f32) -> (Track16, Stems) {
        let pcm = Track16::from(Pcm {
            rate: 44100,
            samples: vec![mix; 8],
        });
        let q = |v: f32| vec![(v * 32768.0) as i16; 8];
        (pcm, Stems::complete([q(0.1), q(0.2), q(0.3), q(0.4)]))
    }

    #[test]
    fn shared_stems_play_the_same_samples_with_independent_deck_gains() {
        use std::sync::Arc;
        let (pcm, stems) = fixture(0.9);
        let (pcm, stems) = (Arc::new(pcm), Arc::new(stems));
        let (other_pcm, other_stems) = (pcm.clone(), stems.clone());
        let mut left = Mixed {
            pcm: &pcm,
            stems: &stems,
            gain: [0.0, 1.0, 1.0, 1.0],
        };
        let right = Mixed {
            pcm: &other_pcm,
            stems: &other_stems,
            gain: [0.0, 0.0, 0.0, 1.0],
        };
        for at in [0.0, 0.5, 1.25, 3.0] {
            assert!((left.frame_at(at).unwrap()[0] - 0.825).abs() < 1e-4);
            assert!((right.frame_at(at).unwrap()[0] - 0.375).abs() < 1e-4);
            let unchanged = right.frame_at(at);
            left.gain = [1.0; COUNT];
            assert_eq!(left.frame_at(at), pcm.frame_at(at));
            assert_eq!(right.frame_at(at), unchanged);
            left.gain = [0.0, 1.0, 1.0, 1.0];
        }
    }

    #[test]
    fn all_on_is_the_original_even_with_a_residual() {
        let (pcm, stems) = fixture(0.9);
        let m = Mixed {
            pcm: &pcm,
            stems: &stems,
            gain: [1.0; COUNT],
        };
        assert_eq!(m.frame_at(1.5), pcm.frame_at(1.5));
    }

    #[test]
    fn muting_removes_that_stem_and_solo_keeps_only_it() {
        let (pcm, stems) = fixture(1.0);
        let mute_vocal = Mixed {
            pcm: &pcm,
            stems: &stems,
            gain: [0.0, 1.0, 1.0, 1.0],
        };
        assert!((mute_vocal.frame_at(0.0).unwrap()[0] - 0.9).abs() < 1e-4);
        let solo_drums = Mixed {
            pcm: &pcm,
            stems: &stems,
            gain: [0.0, 0.0, 0.0, 1.0],
        };
        assert!((solo_drums.frame_at(2.0).unwrap()[1] - 0.4).abs() < 1e-4);
        let all_off = Mixed {
            pcm: &pcm,
            stems: &stems,
            gain: [0.0; COUNT],
        };
        assert_eq!(all_off.frame_at(0.0), Some([0.0, 0.0]));
    }

    #[test]
    fn output_is_continuous_in_the_gains_and_ends_with_the_track() {
        let (pcm, stems) = fixture(0.8); // residual 0.2 of the mix
        let a = |g: f32| {
            Mixed {
                pcm: &pcm,
                stems: &stems,
                gain: [g, 1.0, 1.0, 1.0],
            }
            .frame_at(0.0)
            .unwrap()[0]
        };
        let steps: Vec<f32> = (0..=10).map(|i| a(1.0 - i as f32 / 10.0)).collect();
        assert!(
            steps.windows(2).all(|w| (w[0] - w[1]).abs() < 0.03),
            "{steps:?}"
        );
        assert_eq!(
            Mixed {
                pcm: &pcm,
                stems: &stems,
                gain: [0.0; COUNT]
            }
            .frame_at(4.0),
            None
        );
    }

    /// A track of constant 0.5 split into 4-frame segments overlapping by 2, every stem 0.125 (sum = mix).
    fn grid_fixture(frames: usize) -> (Track16, Stems, Grid) {
        let g = Grid { len: 4, overlap: 2 };
        (
            Pcm {
                rate: 44100,
                samples: vec![0.5; 2 * frames],
            }
            .into(),
            Stems::new(frames, g),
            g,
        )
    }

    fn deposit_all(stems: &Stems, g: Grid, order: &[usize]) {
        let seg = vec![0.125f32; 2 * g.len];
        for &s in order {
            let valid = (stems.frames() - g.start(s)).min(g.len);
            stems.deposit(s, [&seg[..2 * valid]; COUNT]);
        }
    }

    #[test]
    fn a_frame_is_ready_only_when_every_segment_touching_it_is_in() {
        let (_, stems, g) = grid_fixture(10); // 4 segments: 0..4, 2..6, 4..8, 6..10
        assert_eq!(stems.segments(), 4);
        deposit_all(&stems, g, &[2]);
        assert!(
            (0..10).all(|i| !stems.ready(i)),
            "segment 2 alone finishes no frame: both of its halves are shared"
        );
        deposit_all(&stems, g, &[1]);
        assert!(
            (4..6).all(|i| stems.ready(i)),
            "segments 1 and 2 are both in"
        );
        assert!(
            !stems.ready(2) && !stems.ready(3),
            "2..4 is shared with segment 0, which is missing"
        );
        deposit_all(&stems, g, &[0]);
        assert!((0..6).all(|i| stems.ready(i)) && !stems.ready(6) && !stems.is_complete());
        deposit_all(&stems, g, &[3]);
        assert!(stems.is_complete() && (0..10).all(|i| stems.ready(i)));
    }

    #[test]
    fn blending_is_the_same_in_any_order_and_sums_to_the_part() {
        for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2]] {
            let (_, stems, g) = grid_fixture(10);
            deposit_all(&stems, g, &order);
            for k in 0..COUNT {
                let mut v = vec![0i16; 20];
                stems.read(k, 0, &mut v);
                assert!(
                    v.iter().all(|&x| (x - 4096).abs() <= 1),
                    "stem {k} in order {order:?}: {v:?}"
                );
            }
        }
    }

    #[test]
    fn not_ready_frames_play_the_original_and_ready_ones_the_stems() {
        let (pcm, stems, g) = grid_fixture(10);
        let mixed = |gain| Mixed {
            pcm: &pcm,
            stems: &stems,
            gain,
        };
        let drums = [0.0, 0.0, 0.0, 1.0];
        assert_eq!(
            mixed(drums).frame_at(3.0),
            Some([0.5, 0.5]),
            "nothing separated yet: the track"
        );
        deposit_all(&stems, g, &[0, 1]);
        assert!(
            (mixed(drums).frame_at(3.0).unwrap()[0] - 0.125).abs() < 1e-3,
            "ready: drums only"
        );
        assert_eq!(
            mixed(drums).frame_at(7.0),
            Some([0.5, 0.5]),
            "segments 2 and 3 still missing"
        );
    }

    #[test]
    fn a_single_segment_track_and_the_last_segment_cover_every_frame() {
        let (_, stems, g) = grid_fixture(3); // shorter than a segment
        assert_eq!(stems.segments(), 1);
        deposit_all(&stems, g, &[0]);
        assert!((0..3).all(|i| stems.ready(i)));
        let (_, stems, g) = grid_fixture(9); // last segment 6..9 is short
        deposit_all(&stems, g, &[0, 1, 2, 3]);
        assert!((0..9).all(|i| stems.ready(i)));
    }
}
