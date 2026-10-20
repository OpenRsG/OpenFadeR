//! STEM waveform (PREFS `stem_wave`): while stems are ready and some part is muted,
//! soloed or turned down, the detail wave and the overview show what is heard instead of the original track; the beat
//! grid, cues and markers are untouched.
//!
//! The heard audio is the player's own mix ([`Mixed`] with the deck's [`stem gains`](Deck::stem_gains)), analysed with the
//! same 3-band waveform as the original ([`detect::peaks`] / [`detect::quantise`]), but scaled by the ORIGINAL's loudest
//! peak, so a muted part shows as a lower wave and all gains at 1 give exactly the original analysis. That is always the
//! 3-band look, whatever the PREFS waveform colour mode (the other modes have no analysis of our own to run on the mix).
//!
//! It is computed off the UI thread ([`stem_wave`] on a worker, one in flight per deck) once the gains have been
//! steady for [`SETTLE`], so dragging an isolator knob does not recompute at every step.
//! The deck keeps copies of the track audio and the stems for that (memory, nothing on disk) and parks the original waves
//! while the stem wave is shown, so going back is instant. Both are dropped with the track (LOAD) or when STEMS goes off.

use super::Deck;
use crate::app::render;
use openfader::analysis::detect;
use openfader::audio::{Mixed, STEM_COUNT, Source, Stems, Track16};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering::Relaxed},
    mpsc::{Receiver, TryRecvError, channel},
};
use std::time::{Duration, Instant};

/// How long the gains must stay as they are before the stem wave is recomputed.
const SETTLE: Duration = Duration::from_millis(150);

type Gains = [f32; STEM_COUNT];
/// `(detail, preview)` as [`detect::waveform`] returns them.
pub type Bands = (Vec<[u8; 3]>, Vec<[u8; 3]>);

fn peaks_cancellable(
    frames: impl Iterator<Item = [f32; 2]>,
    n: usize,
    rate: u32,
    cancel: &AtomicBool,
) -> Option<Vec<[f32; 3]>> {
    if cancel.load(Relaxed) {
        return None;
    }
    let frames = frames
        .enumerate()
        .map_while(|(i, f)| (!(i % 4096 == 0 && cancel.load(Relaxed))).then_some(f));
    let peaks = detect::peaks_of(frames, n, rate);
    (!cancel.load(Relaxed)).then_some(peaks)
}

/// `top` belongs to this exact quantized PCM, independent of stem gains.
fn stem_wave(
    pcm: &Track16,
    stems: &Stems,
    gain: Gains,
    top: &OnceLock<f32>,
    cancel: &AtomicBool,
) -> Option<Bands> {
    let n = pcm.frames();
    let scale = match top.get() {
        Some(&scale) => scale,
        None => {
            let original = (0..n).map(|i| pcm.frame_at(i as f64).unwrap_or_default());
            let scale = detect::loudest(&peaks_cancellable(original, n, pcm.rate, cancel)?);
            let _ = top.set(scale);
            scale
        }
    };
    let mixed = Mixed { pcm, stems, gain };
    let heard = (0..n).map(|i| mixed.frame_at(i as f64).unwrap_or_default());
    let peaks = peaks_cancellable(heard, n, pcm.rate, cancel)?;
    let bands = detect::quantise(&peaks, scale);
    (!cancel.load(Relaxed)).then_some(bands)
}

struct Worker {
    rx: Receiver<Option<Bands>>,
    gain: Gains,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Relaxed);
    }
}

/// What a deck keeps of the STEM waveform.
#[derive(Default)]
pub(super) struct State {
    pcm: Option<Arc<Track16>>,
    top: Arc<OnceLock<f32>>,
    generation: u64,
    stems: Option<Arc<Stems>>,
    /// The gains the stem wave is wanted for (`None`: the original) and since when.
    want: Option<Gains>,
    since: Option<Instant>,
    /// The gains of the stem wave on show (`None`: the original), whose original waves wait in `orig` (detail, overview).
    shown: Option<Gains>,
    dirty: bool,
    pub(super) orig: Option<(Option<render::Wave>, Option<render::Wave>)>,
    /// The worker computing the wave for these gains.
    job: Option<Worker>,
}

impl State {
    fn set_pcm(&mut self, pcm: &Arc<Track16>) {
        if self.pcm.as_ref().is_some_and(|old| Arc::ptr_eq(old, pcm)) {
            return;
        }
        self.invalidate_worker();
        self.top = Arc::new(OnceLock::new());
        self.pcm = Some(pcm.clone());
    }

    fn invalidate_worker(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(job) = &self.job {
            job.cancel.store(true, Relaxed);
        }
    }

    fn clear(&mut self) {
        self.invalidate_worker();
        let (job, generation) = (self.job.take(), self.generation);
        *self = State {
            job,
            generation,
            ..State::default()
        };
    }

    fn take_finished(&mut self) -> Option<(Bands, Gains)> {
        let job = self.job.as_ref()?;
        let (gain, generation) = (job.gain, job.generation);
        match job.rx.try_recv() {
            Ok(bands) => {
                self.job = None;
                bands
                    .filter(|_| generation == self.generation)
                    .map(|bands| (bands, gain))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                None
            }
        }
    }
}

impl Deck {
    /// The audio the stems were separated from (the player's own copy, shared).
    pub(super) fn stemwave_pcm(&self, pcm: &Arc<Track16>) {
        self.stemwave.borrow_mut().set_pcm(pcm);
    }

    /// The separated stems (the player holds the same copy).
    pub(super) fn stemwave_stems(&self, stems: &Arc<Stems>) {
        let mut st = self.stemwave.borrow_mut();
        if st.stems.as_ref().is_some_and(|old| Arc::ptr_eq(old, stems)) {
            return;
        }
        st.invalidate_worker();
        st.dirty = true;
        st.stems = Some(stems.clone());
    }

    /// A new track replaces the old (its waves go with it, see `install`).
    pub(super) fn stemwave_clear(&self) {
        self.stemwave.borrow_mut().clear();
    }

    /// Tick: takes a finished wave, goes back to the original, and starts the worker for the gains wanted.
    pub(super) fn stemwave_tick(&self) {
        let p = super::tick::opts();
        let ready = self.stem_gains(p.stems_four);
        let want = ready.filter(|g| p.stem_wave && g.iter().any(|&g| g != 1.0));
        let mut st = self.stemwave.borrow_mut();
        if !p.stems && st.pcm.take().is_some() {
            st.top = Arc::new(OnceLock::new());
        }
        if ready.is_none() {
            st.stems = None;
        }
        if st.want != want {
            st.invalidate_worker();
            (st.want, st.since) = (want, Some(Instant::now()));
        }
        if let Some(((detail, preview), g)) = st.take_finished() {
            if want == Some(g) {
                let mut l = self.loaded.borrow_mut();
                let prev = (l.detail.take(), l.overview.take());
                l.detail = render::detail::wave_3band(&detail);
                l.overview = render::overview::wave_3band(&preview, l.duration_ms);
                if st.shown.is_none() {
                    st.orig = Some(prev);
                }
                st.shown = Some(g);
                st.dirty = false;
                l.invalidate();
            }
        }
        let Some(g) = want else {
            if let (Some(_), Some((detail, overview))) = (st.shown.take(), st.orig.take()) {
                let mut l = self.loaded.borrow_mut();
                (l.detail, l.overview) = (detail, overview);
                l.invalidate();
            }
            return;
        };
        let settled = st.since.is_some_and(|t| t.elapsed() >= SETTLE);
        if let (true, true, None, Some(pcm), Some(stems)) = (
            st.shown != want || st.dirty,
            settled,
            &st.job,
            &st.pcm,
            &st.stems,
        ) {
            let (pcm, stems, top, (tx, rx)) =
                (pcm.clone(), stems.clone(), st.top.clone(), channel());
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            std::thread::spawn(move || {
                let _ = tx.send(stem_wave(&pcm, &stems, g, &top, &worker_cancel));
            });
            st.job = Some(Worker {
                rx,
                gain: g,
                generation: st.generation,
                cancel,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::DETAIL_ENTRIES_PER_SEC;
    use openfader::audio::{DRUMS, Pcm, VOCAL};

    fn stem_wave(pcm: &Track16, stems: &Stems, gain: Gains) -> Bands {
        super::stem_wave(pcm, stems, gain, &OnceLock::new(), &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn cached_scale_matches_fresh_original_normalization_for_every_gain() {
        let (pcm, stems) = fixture();
        let top = OnceLock::new();
        let original = detect::peaks(&samples_of(&pcm), pcm.rate);
        let scale = detect::loudest(&original);
        for gain in [
            [0.0, 1.0, 0.4, 1.0],
            [1.0, 0.0, 0.0, 0.2],
            [1.0; STEM_COUNT],
        ] {
            let mixed = Mixed {
                pcm: &pcm,
                stems: &stems,
                gain,
            };
            let heard = (0..pcm.frames()).map(|i| mixed.frame_at(i as f64).unwrap());
            let expected =
                detect::quantise(&detect::peaks_of(heard, pcm.frames(), pcm.rate), scale);
            assert_eq!(
                super::stem_wave(&pcm, &stems, gain, &top, &AtomicBool::new(false)),
                Some(expected)
            );
            assert_eq!(top.get(), Some(&scale));
        }
    }

    #[test]
    fn replacing_pcm_recomputes_scale_from_its_quantized_original() {
        let (pcm, stems) = fixture();
        let quiet = Arc::new(Track16::from(Pcm {
            rate: pcm.rate,
            samples: samples_of(&pcm).iter().map(|x| x * 0.25).collect(),
        }));
        let loud = Arc::new(pcm);
        let mut state = State::default();
        let mut previous = None;
        for pcm in [&quiet, &loud] {
            state.set_pcm(pcm);
            let expected = detect::waveform(&samples_of(pcm), pcm.rate);
            assert_eq!(
                super::stem_wave(pcm, &stems, [1.0; 4], &state.top, &AtomicBool::new(false)),
                Some(expected)
            );
            let scale = *state.top.get().unwrap();
            if let Some(previous) = previous {
                assert!(scale > previous * 3.9);
            }
            previous = Some(scale);
            state.set_pcm(pcm);
            assert_eq!(state.top.get(), Some(&scale));
        }
    }

    #[test]
    #[ignore = "timing comparison; run explicitly with --nocapture"]
    fn bench_cached_stemwave_normalization() {
        let (pcm, stems) = fixture();
        let top = OnceLock::new();
        let cancel = AtomicBool::new(false);
        let gain = [0.0, 1.0, 1.0, 1.0];
        let expected = super::stem_wave(&pcm, &stems, gain, &top, &cancel).unwrap();
        let fresh = Instant::now();
        for _ in 0..8 {
            assert_eq!(
                super::stem_wave(&pcm, &stems, gain, &OnceLock::new(), &cancel),
                Some(expected.clone())
            );
        }
        let fresh = fresh.elapsed();
        let cached = Instant::now();
        for _ in 0..8 {
            assert_eq!(
                super::stem_wave(&pcm, &stems, gain, &top, &cancel),
                Some(expected.clone())
            );
        }
        eprintln!(
            "stemwave 8 x 4 s: fresh={fresh:?}, cached={:?}",
            cached.elapsed()
        );
    }

    #[test]
    fn cancelled_peak_scan_stops_and_never_publishes_partial_bands() {
        use std::cell::Cell;
        let (cancel, count) = (AtomicBool::new(false), Cell::new(0));
        let frames = (0..100_000).map(|i| {
            count.set(count.get() + 1);
            if i == 100 {
                cancel.store(true, Relaxed);
            }
            [0.5; 2]
        });
        assert!(peaks_cancellable(frames, 100_000, RATE, &cancel).is_none());
        assert!(count.get() <= 4097);
        let (pcm, stems) = fixture();
        let top = OnceLock::new();
        assert!(super::stem_wave(&pcm, &stems, [0.0; 4], &top, &cancel).is_none());
        assert!(top.get().is_none());
    }

    #[test]
    fn clearing_keeps_the_cancelled_worker_slot_until_it_finishes_and_rejects_stale_output() {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let gain = [0.0; STEM_COUNT];
        let mut state = State::default();
        state.job = Some(Worker {
            rx,
            gain,
            generation: state.generation,
            cancel: cancel.clone(),
        });
        state.clear();
        assert!(cancel.load(Relaxed));
        assert!(state.job.is_some());
        assert!(state.take_finished().is_none());
        assert!(
            state.job.is_some(),
            "no replacement worker until cancellation completes"
        );
        tx.send(Some((vec![[127; 3]], vec![[127; 3]]))).unwrap();
        assert!(
            state.take_finished().is_none(),
            "same gains from an old track must not be shown"
        );
        assert!(state.job.is_none());
        let (tx, rx) = channel();
        state.job = Some(Worker {
            rx,
            gain,
            generation: state.generation,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        let bands = (vec![[17; 3]], vec![[29; 3]]);
        tx.send(Some(bands.clone())).unwrap();
        assert_eq!(state.take_finished(), Some((bands, gain)));
    }

    const RATE: u32 = 44100;
    const FRAMES: usize = 4 * RATE as usize;
    const HALF: usize = 2 * DETAIL_ENTRIES_PER_SEC as usize;

    fn samples_of(pcm: &Track16) -> Vec<f32> {
        pcm.iter_f32(0..pcm.frames()).collect()
    }

    /// 4 s: the vocal is a loud 60 Hz tone in the first half, the drums a loud 8 kHz burst in the second; the track is their
    /// exact sum (no residual), the stems are the same i16 values.
    fn fixture() -> (Track16, Stems) {
        let tone = |hz: f32, i: usize| {
            ((std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin() * 0.5 * 32767.0) as i16
        };
        let part = |hz: f32, on: std::ops::Range<usize>| -> Vec<i16> {
            (0..FRAMES)
                .flat_map(|i| [if on.contains(&i) { tone(hz, i) } else { 0 }; 2])
                .collect()
        };
        let (vocal, drums) = (part(60.0, 0..FRAMES / 2), part(8000.0, FRAMES / 2..FRAMES));
        let samples = vocal
            .iter()
            .zip(&drums)
            .map(|(&v, &d)| (v as f32 + d as f32) / 32768.0)
            .collect();
        let none = vec![0i16; 2 * FRAMES];
        let mut data = [none.clone(), none.clone(), none.clone(), none];
        data[VOCAL] = vocal;
        data[DRUMS] = drums;
        (
            Track16::from(Pcm {
                rate: RATE,
                samples,
            }),
            Stems::complete(data),
        )
    }

    #[test]
    fn muting_the_vocal_empties_its_half_and_keeps_the_rest() {
        let (pcm, stems) = fixture();
        let (orig, _) = detect::waveform(&samples_of(&pcm), RATE);
        let (heard, preview) = stem_wave(&pcm, &stems, [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(heard.len(), orig.len());
        let loud = |w: &[[u8; 3]]| w.iter().flatten().map(|&v| v as u32).max().unwrap();
        assert!(loud(&orig[..HALF - 10]) > 100, "the original has the vocal");
        assert!(
            loud(&heard[..HALF - 10]) <= 1,
            "vocal muted: first half is empty, got {}",
            loud(&heard[..HALF - 10])
        );
        // second half: the original, same scale (the drums stay as loud as they were)
        for (h, o) in heard[HALF + 10..].iter().zip(&orig[HALF + 10..]) {
            assert!(
                h.iter().zip(o).all(|(&a, &b)| a.abs_diff(b) <= 1),
                "{h:?} vs {o:?}"
            );
        }
        assert!(
            preview[..preview.len() / 2 - 5]
                .iter()
                .flatten()
                .all(|&v| v <= 1),
            "the overview agrees"
        );
    }

    #[test]
    fn soloing_the_vocal_leaves_only_the_first_half_and_all_on_is_the_original() {
        let (pcm, stems) = fixture();
        let solo = stem_wave(&pcm, &stems, [1.0, 0.0, 0.0, 0.0]).0;
        assert!(solo[HALF + 10..].iter().flatten().all(|&v| v <= 1));
        assert!(solo[..HALF - 10].iter().flatten().any(|&v| v > 100));
        assert_eq!(
            stem_wave(&pcm, &stems, [1.0; STEM_COUNT]),
            detect::waveform(&samples_of(&pcm), RATE)
        );
    }
}
