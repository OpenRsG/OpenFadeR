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
    Arc,
    mpsc::{Receiver, TryRecvError, channel},
};
use std::time::{Duration, Instant};

/// How long the gains must stay as they are before the stem wave is recomputed.
const SETTLE: Duration = Duration::from_millis(150);

type Gains = [f32; STEM_COUNT];
/// `(detail, preview)` as [`detect::waveform`] returns them.
pub type Bands = (Vec<[u8; 3]>, Vec<[u8; 3]>);

/// The 3-band waveform of `pcm` as heard with the stem `gain`s (0..=1; the player's mix), scaled like the original's.
pub fn stem_wave(pcm: &Track16, stems: &Stems, gain: Gains) -> Bands {
    let mixed = Mixed { pcm, stems, gain };
    let n = pcm.frames();
    let heard = (0..n).map(|i| mixed.frame_at(i as f64).unwrap_or_default());
    let original = (0..n).map(|i| pcm.frame_at(i as f64).unwrap_or_default());
    let top = detect::loudest(&detect::peaks_of(original, n, pcm.rate));
    detect::quantise(&detect::peaks_of(heard, n, pcm.rate), top)
}

/// What a deck keeps of the STEM waveform.
#[derive(Default)]
pub(super) struct State {
    pcm: Option<Arc<Track16>>,
    stems: Option<Arc<Stems>>,
    /// The gains the stem wave is wanted for (`None`: the original) and since when.
    want: Option<Gains>,
    since: Option<Instant>,
    /// The gains of the stem wave on show (`None`: the original), whose original waves wait in `orig` (detail, overview).
    shown: Option<Gains>,
    pub(super) orig: Option<(Option<render::Wave>, Option<render::Wave>)>,
    /// The worker computing the wave for these gains.
    job: Option<(Receiver<Bands>, Gains)>,
}

impl Deck {
    /// The audio the stems were separated from (the player's own copy, shared).
    pub(super) fn stemwave_pcm(&self, pcm: &Arc<Track16>) {
        self.stemwave.borrow_mut().pcm = Some(pcm.clone());
    }

    /// The separated stems (the player holds the same copy).
    pub(super) fn stemwave_stems(&self, stems: &Arc<Stems>) {
        self.stemwave.borrow_mut().stems = Some(stems.clone());
    }

    /// A new track replaces the old (its waves go with it, see `install`).
    pub(super) fn stemwave_clear(&self) {
        *self.stemwave.borrow_mut() = State::default();
    }

    /// Tick: takes a finished wave, goes back to the original, and starts the worker for the gains wanted.
    pub(super) fn stemwave_tick(&self) {
        let p = super::tick::opts();
        let ready = self.stem_gains(p.stems_four);
        let want = ready.filter(|g| p.stem_wave && g.iter().any(|&g| g != 1.0));
        let mut st = self.stemwave.borrow_mut();
        if !p.stems {
            st.pcm = None;
        }
        if ready.is_none() {
            st.stems = None;
        }
        if st.want != want {
            (st.want, st.since) = (want, Some(Instant::now()));
        }
        let arrived = match st.job.as_ref().map(|(rx, g)| (rx.try_recv(), *g)) {
            Some((Ok(bands), g)) => Some((bands, g)),
            Some((Err(TryRecvError::Empty), _)) => None,
            Some((Err(TryRecvError::Disconnected), _)) => {
                st.job = None;
                None
            }
            None => None,
        };
        if let Some(((detail, preview), g)) = arrived {
            st.job = None;
            if want == Some(g) {
                let mut l = self.loaded.borrow_mut();
                let prev = (l.detail.take(), l.overview.take());
                l.detail = render::detail::wave_3band(&detail);
                l.overview = render::overview::wave_3band(&preview, l.duration_ms);
                if st.shown.is_none() {
                    st.orig = Some(prev);
                }
                st.shown = Some(g);
                l.invalidate();
            }
        }
        let Some(g) = want else {
            st.job = None;
            if let (Some(_), Some((detail, overview))) = (st.shown.take(), st.orig.take()) {
                let mut l = self.loaded.borrow_mut();
                (l.detail, l.overview) = (detail, overview);
                l.invalidate();
            }
            return;
        };
        let settled = st.since.is_some_and(|t| t.elapsed() >= SETTLE);
        if let (true, true, None, Some(pcm), Some(stems)) =
            (st.shown != want, settled, &st.job, &st.pcm, &st.stems)
        {
            let (pcm, stems, (tx, rx)) = (pcm.clone(), stems.clone(), channel());
            std::thread::spawn(move || {
                let _ = tx.send(stem_wave(&pcm, &stems, g));
            });
            st.job = Some((rx, g));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::DETAIL_ENTRIES_PER_SEC;
    use openfader::audio::{DRUMS, Pcm, VOCAL};

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
