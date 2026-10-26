//! Separation of a track by segments: [`grid`] cuts it, [`segment`] feeds the fixed-length model one overlapping piece and
//! deposits the stems at the track's own rate, [`work`] is one engine's loop (take the next segment from the
//! [`Schedule`](super::schedule::Schedule), run it, deposit it). Any number of engines run
//! `work` side by side on the same [`Stems`]; the cross-fades live with the stems, which is why the order of segments
//! does not matter and playback can start on the first one ([`crate::audio::Stems::ready`]).
//!
//! The model wants 44.1 kHz; a track at another rate is read with windowed-sinc resampling and its stems resampled back the same
//! way, segment by segment, so the stems always have the track's own rate and frame count.

use super::Quality;
use super::model::{self, SEGMENT};
use super::schedule::Schedule;
use crate::audio::{Grid, STEM_COUNT, Stems, Track16};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Frames of the track per model sample.
fn ratio(rate: u32) -> f64 {
    rate as f64 / model::RATE as f64
}

/// The segments of a track at `rate` Hz separated at `quality`: the model's segment and the quality's overlap, in the track's
/// own frames.
pub fn grid(rate: u32, quality: Quality) -> Grid {
    let scale = |n: usize| (n as f64 * ratio(rate)).round() as usize;
    Grid {
        len: scale(SEGMENT).max(1),
        overlap: scale(quality.overlap()).max(1),
    }
}

/// Runs a model segment and deposits its borrowed output directly at the track's rate.
pub fn segment(
    pcm: &Track16,
    stems: &Stems,
    s: usize,
    run: &mut dyn FnMut(
        Vec<f32>,
        &mut dyn FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String>,
) -> Result<(), String> {
    let g = stems.grid();
    let (start, step) = (g.start(s), ratio(pcm.rate));
    let valid = g.len.min(pcm.frames().saturating_sub(start));
    let mut x = vec![0f32; 2 * SEGMENT];
    for i in 0..SEGMENT {
        match pcm.frame_at_step(start as f64 + i as f64 * step, step) {
            Some([l, r]) => (x[i], x[SEGMENT + i]) = (l, r),
            None => break,
        }
    }
    run(x, &mut |y| {
        if y.len() != STEM_COUNT * 2 * SEGMENT {
            return Err(format!(
                "model returned {} values, wanted {}",
                y.len(),
                STEM_COUNT * 2 * SEGMENT
            ));
        }
        stems.deposit_frames(s, valid, |t| {
            let at = t as f64 / step;
            let (i, f) = (at as usize, (at - at.floor()) as f32);
            let (i, j) = (i.min(SEGMENT - 1), (i + 1).min(SEGMENT - 1));
            let mut parts = [[0.0; 2]; STEM_COUNT];
            for (m, &stem) in model::ORDER.iter().enumerate() {
                for c in 0..2 {
                    let ch = &y[(2 * m + c) * SEGMENT..(2 * m + c + 1) * SEGMENT];
                    parts[stem][c] = ch[i] + (ch[j] - ch[i]) * f;
                }
            }
            parts
        });
        Ok(())
    })
}

/// One engine's loop: segments from `sched` through `run` into `stems` until none are left, `cancel` is set (checked between
/// segments) or `run` fails. `done` is called after each segment with its number.
pub fn work(
    pcm: &Track16,
    stems: &Stems,
    sched: &Mutex<Schedule>,
    run: &mut dyn FnMut(
        Vec<f32>,
        &mut dyn FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String>,
    cancel: &AtomicBool,
    done: &dyn Fn(usize),
) -> Result<(), String> {
    while !cancel.load(Relaxed) {
        let Some(s) = sched.lock().next() else {
            return Ok(());
        };
        if let Err(e) = segment(pcm, stems, s, run) {
            sched.lock().release(s); // another engine takes it
            return Err(e);
        }
        done(s);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{BASS, DRUMS, INST, Pcm, VOCAL};
    use std::sync::atomic::AtomicUsize;

    /// A stand-in for the model: vocals = 0.4·mix, inst = 0.3, bass = 0.2, drums = 0.1 (sums to the mix).
    fn fake(
        mix: Vec<f32>,
        consume: &mut dyn FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut y = Vec::with_capacity(4 * mix.len());
        for share in [0.1, 0.2, 0.3, 0.4] {
            // the model's order: drums, bass, other, vocals
            y.extend(mix.iter().map(|v| v * share));
        }
        consume(&y)
    }

    fn tone(rate: u32, frames: usize) -> Track16 {
        let s = (0..frames).flat_map(|i| {
            let v = ((i as f32) * 0.05).sin() * 0.5;
            [v, -v]
        });
        Track16::from(Pcm {
            rate,
            samples: s.collect(),
        })
    }

    #[test]
    fn direct_deposit_is_bit_exact_to_interleaved_segments_at_all_rates_and_qualities() {
        let y: Vec<f32> = (0..STEM_COUNT * 2 * SEGMENT)
            .map(|i| (i as f32 * 0.013).sin() * 1.3)
            .collect();
        for rate in [22_050, 44_100, 48_000, 96_000] {
            for quality in [Quality::Quality, Quality::Speed] {
                let g = grid(rate, quality);
                let pcm = tone(rate, g.len + g.stride() + 17);
                for reverse in [false, true] {
                    let direct = Stems::new(pcm.frames(), g);
                    let mut reference: [Vec<i16>; STEM_COUNT] =
                        std::array::from_fn(|_| vec![0; pcm.frames() * 2]);
                    let count = direct.segments();
                    let mut deposited = vec![false; count];
                    for n in 0..count {
                        let s = if reverse { count - 1 - n } else { n };
                        segment(&pcm, &direct, s, &mut |_, consume| consume(&y)).unwrap();
                        let valid = g.len.min(pcm.frames() - g.start(s));
                        let mut parts: [Vec<f32>; STEM_COUNT] =
                            std::array::from_fn(|_| vec![0.0; valid * 2]);
                        for t in 0..valid {
                            let at = t as f64 / ratio(rate);
                            let (i, f) = (at as usize, (at - at.floor()) as f32);
                            let (i, j) = (i.min(SEGMENT - 1), (i + 1).min(SEGMENT - 1));
                            for (m, &stem) in model::ORDER.iter().enumerate() {
                                for c in 0..2 {
                                    let ch = &y[(2 * m + c) * SEGMENT..(2 * m + c + 1) * SEGMENT];
                                    parts[stem][2 * t + c] = ch[i] + (ch[j] - ch[i]) * f;
                                }
                            }
                        }
                        // Independent scalar reference: each overlap rounds once per segment contribution.
                        let (first, last) = (s == 0, s + 1 == count);
                        for t in 0..valid {
                            let ramp = (g.overlap + 1) as f32;
                            let up = if first || t >= g.overlap {
                                1.0
                            } else {
                                (t + 1) as f32 / ramp
                            };
                            let down = if last || t < g.stride() {
                                1.0
                            } else {
                                (g.len - t) as f32 / ramp
                            };
                            let weight = up * down * 32768.0;
                            let add = if !first && t < g.overlap {
                                deposited[s - 1]
                            } else if !last && t >= g.stride() {
                                deposited[s + 1]
                            } else {
                                false
                            };
                            for k in 0..STEM_COUNT {
                                for c in 0..2 {
                                    let cell = &mut reference[k][2 * (g.start(s) + t) + c];
                                    let value = parts[k][2 * t + c] * weight
                                        + if add { *cell as f32 } else { 0.0 };
                                    *cell = value.round().clamp(-32768.0, 32767.0) as i16;
                                }
                            }
                        }
                        deposited[s] = true;
                    }
                    assert!(direct.is_complete());
                    for k in 0..STEM_COUNT {
                        let mut got = vec![0; pcm.frames() * 2];
                        direct.read(k, 0, &mut got);
                        assert_eq!(
                            got, reference[k],
                            "rate={rate} quality={quality:?} reverse={reverse} stem={k}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn malformed_output_never_marks_a_segment_ready() {
        let pcm = tone(44_100, 100);
        let stems = Stems::new(pcm.frames(), grid(pcm.rate, Quality::Speed));
        let error = segment(&pcm, &stems, 0, &mut |_, consume| consume(&[0.0])).unwrap_err();
        assert!(error.contains("model returned 1 values"));
        assert_eq!(stems.segments_done(), 0);
        assert!(!stems.ready(0));
    }

    #[test]
    fn grids_cover_the_model_segment_at_every_rate_and_speed_runs_fewer_segments() {
        let q = grid(44_100, Quality::Quality);
        assert_eq!((q.len, q.overlap), (SEGMENT, Quality::Quality.overlap()));
        let s = grid(44_100, Quality::Speed);
        assert!(s.count(10_000_000) < q.count(10_000_000));
        let g48 = grid(48_000, Quality::Quality);
        assert_eq!(
            g48.len,
            (SEGMENT as f64 * 48_000.0 / 44_100.0).round() as usize
        );
    }

    #[test]
    fn engines_split_the_track_and_the_stems_sum_to_the_mix_in_any_order() {
        let pcm = tone(44_100, 3 * SEGMENT + 12_345);
        let frames = pcm.frames();
        let g = grid(44_100, Quality::Quality);
        let stems = Stems::new(frames, g);
        let sched = Mutex::new(Schedule::new(g, stems.segments(), false));
        sched.lock().focus(2 * SEGMENT, &[]); // play near the end: the last segments first
        let (cancel, count) = (AtomicBool::new(false), AtomicUsize::new(0));
        let first = Mutex::new(None);
        std::thread::scope(|sc| {
            for _ in 0..2 {
                sc.spawn(|| {
                    work(&pcm, &stems, &sched, &mut fake, &cancel, &|s| {
                        first.lock().get_or_insert(s);
                        count.fetch_add(1, Relaxed);
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(count.load(Relaxed), stems.segments());
        assert!(stems.is_complete() && (0..frames).step_by(997).all(|i| stems.ready(i)));
        let mut parts = [(); 4].map(|_| vec![0i16; 2 * frames]);
        for (k, p) in parts.iter_mut().enumerate() {
            stems.read(k, 0, p);
        }
        let worst = (0..2 * frames)
            .map(|i| {
                let sum: f32 = parts.iter().map(|p| p[i] as f32).sum();
                (sum / 32768.0 - pcm.frame_at((i / 2) as f64).unwrap()[i % 2]).abs()
            })
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "Σ stems differs from the mix by {worst}");
        let share = |k: usize| parts[k].iter().map(|&v| (v as f32).abs()).sum::<f32>();
        assert!(
            (share(VOCAL) / share(DRUMS) - 4.0).abs() < 0.05
                && (share(INST) / share(BASS) - 1.5).abs() < 0.05
        );
    }

    #[test]
    fn the_play_position_is_playable_after_one_segment_not_after_the_whole_track() {
        let pcm = tone(44_100, 6 * SEGMENT);
        let g = grid(44_100, Quality::Speed);
        let stems = Stems::new(pcm.frames(), g);
        let sched = Mutex::new(Schedule::new(g, stems.segments(), false));
        let play = 4 * SEGMENT + SEGMENT / 2;
        sched.lock().focus(play, &[]);
        let cancel = AtomicBool::new(false);
        let stop_after_one = |_: usize| cancel.store(true, Relaxed);
        work(&pcm, &stems, &sched, &mut fake, &cancel, &stop_after_one).unwrap();
        assert_eq!(stems.segments_done(), 1);
        assert!(!stems.is_complete() && stems.segment_done(g.segment_of(play, stems.segments())));
    }

    #[test]
    fn a_track_at_48k_keeps_its_rate_and_length() {
        let pcm = tone(48_000, 48_000 * 20);
        let g = grid(48_000, Quality::Speed);
        let stems = Stems::new(pcm.frames(), g);
        let sched = Mutex::new(Schedule::new(g, stems.segments(), true));
        work(
            &pcm,
            &stems,
            &sched,
            &mut fake,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        assert!(stems.is_complete());
        let mut inst = vec![0i16; 2 * 1000];
        stems.read(INST, 480_000, &mut inst);
        let at = |f: usize| pcm.frame_at(f as f64).unwrap()[0];
        assert!((inst[2 * 500] as f32 / 32768.0 - 0.3 * at(480_500)).abs() < 2e-3);
    }

    #[test]
    fn a_failing_engine_stops_its_loop_with_the_error() {
        let pcm = tone(44_100, SEGMENT);
        let g = grid(44_100, Quality::Quality);
        let stems = Stems::new(pcm.frames(), g);
        let sched = Mutex::new(Schedule::new(g, 1, true));
        let e = work(
            &pcm,
            &stems,
            &sched,
            &mut |_, _| Err("no GPU".into()),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap_err();
        assert_eq!(e, "no GPU");
        assert_eq!(stems.segments_done(), 0);
        assert_eq!(
            sched.lock().next(),
            Some(0),
            "the segment it dropped is handed out again"
        );
    }

    #[test]
    fn an_engine_that_fails_hands_its_segment_to_the_other_and_the_track_still_completes() {
        let pcm = tone(44_100, 2 * SEGMENT + 5000);
        let g = grid(44_100, Quality::Speed);
        let stems = Stems::new(pcm.frames(), g);
        let sched = Mutex::new(Schedule::new(g, stems.segments(), true));
        let cancel = AtomicBool::new(false);
        let gpu = work(
            &pcm,
            &stems,
            &sched,
            &mut |_, _| Err("device lost".into()),
            &cancel,
            &|_| {},
        );
        assert_eq!(gpu, Err("device lost".into()));
        work(&pcm, &stems, &sched, &mut fake, &cancel, &|_| {}).unwrap();
        assert!(stems.is_complete() && (0..stems.frames()).step_by(499).all(|i| stems.ready(i)));
    }

    #[test]
    fn muting_a_stem_lowers_the_mix_by_its_share_and_solo_keeps_only_that_stem() {
        use crate::audio::{Mixed, Source};
        let pcm = tone(44_100, SEGMENT + 20_000);
        let frames = pcm.frames();
        let g = grid(44_100, Quality::Speed);
        let stems = Stems::new(frames, g);
        let sched = Mutex::new(Schedule::new(g, stems.segments(), true));
        work(
            &pcm,
            &stems,
            &sched,
            &mut fake,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        let rms = |gain: [f32; 4]| {
            let m = Mixed {
                pcm: &pcm,
                stems: &stems,
                gain,
            };
            let e: f64 = (0..frames)
                .map(|i| m.frame_at(i as f64).unwrap()[0].powi(2) as f64)
                .sum();
            (e / frames as f64).sqrt()
        };
        let all = rms([1.0; 4]);
        assert!(
            (rms([0.0, 1.0, 1.0, 1.0]) / all - 0.6).abs() < 0.01,
            "vocal is 0.4 of the mix"
        );
        assert!(
            (rms([1.0, 1.0, 1.0, 0.0]) / all - 0.9).abs() < 0.01,
            "drums are 0.1"
        );
        assert!(
            (rms([0.0, 0.0, 0.0, 1.0]) / all - 0.1).abs() < 0.01,
            "drums alone"
        );
        assert!(
            (rms([1.0, 0.0, 0.0, 0.0]) / all - 0.4).abs() < 0.01,
            "vocal alone"
        );
    }
}
