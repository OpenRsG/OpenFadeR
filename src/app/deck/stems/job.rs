//! The separation job of a deck: start, focus, progress polling and the PREFS reaction.

use super::super::{Deck, PadMode};
use crate::app::prefs;
use openfader::audio::{self, stemfile};
use openfader::stems::{Phase, Quality};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};
use std::{path::PathBuf, time::Instant};

fn progress_text(phase: Phase, f: f32) -> String {
    let pct = (f * 100.0) as u32;
    match phase {
        Phase::Opening => tr!("STEMS 0%").into(),
        Phase::Download => tr!("MODEL {}%", pct),
        Phase::Separate => tr!("STEMS {}%", pct),
    }
}

fn compatible_stems(stems: &audio::Stems, pcm: &audio::Track16, fast: bool) -> bool {
    let quality = openfader::stems::separate::grid(pcm.rate, Quality::Quality);
    let speed = openfader::stems::separate::grid(pcm.rate, Quality::Speed);
    stems.is_complete()
        && stems.frames() == pcm.frames()
        && (stems.grid() == quality || (fast && stems.grid() == speed))
}

static FIRST_TICK: LazyLock<Instant> = LazyLock::new(Instant::now);
/// The engines were opened (a warm-up or a job) and not released since.
static WARM: AtomicBool = AtomicBool::new(false);
/// Seconds after [`FIRST_TICK`] of the last tick with a job running, or of the last warm-up.
static LAST_BUSY: AtomicU64 = AtomicU64::new(0);
/// No separation on any deck for this long closes the engines (about 1 GB with both open).
const IDLE_S: u64 = 90;

fn now_s() -> u64 {
    FIRST_TICK.elapsed().as_secs()
}

/// Every deck tick (`busy`: this deck has a job): after [`IDLE_S`] without a job on any deck, the engines are closed, off the
/// UI thread (closing waits for a segment in flight). The next load opens them again ([`Deck::stems_warm`]).
fn engines_tick(busy: bool) {
    let now = now_s();
    if busy {
        LAST_BUSY.store(now, Relaxed);
        WARM.store(true, Relaxed);
    } else if WARM.load(Relaxed) && now.saturating_sub(LAST_BUSY.load(Relaxed)) >= IDLE_S {
        WARM.store(false, Relaxed);
        std::thread::spawn(openfader::stems::model::release);
    }
}

impl Deck {
    /// A track starts decoding: with STEMS on the engines open meanwhile (2 to 3 s, overlapping the decode), so an idle app
    /// holds no model. Not in the first 2 s of the event loop: Dawn loading the Vulkan driver while the main thread loads GLX
    /// (the same NVIDIA libraries) hangs the window for good (seen: main thread busy in `dlopen`, no window); a job
    /// started that early opens them itself.
    pub(in crate::app::deck) fn stems_warm(&self, file: Option<&std::path::Path>) {
        if WARM.load(Relaxed) || now_s() < 2 {
            return;
        }
        prefs::with(|p| {
            let quality = if p.stems_fast {
                Quality::Speed
            } else {
                Quality::Quality
            };
            // A cached track separates nothing: the job reads the entry, so no engine is needed. Nor does a stem file.
            let separates = !file.is_some_and(|f| {
                openfader::stems::cache::has(f, quality)
                    || p.extensions.stem_files && stemfile::read_info(f).is_some()
            });
            if p.stems && separates {
                WARM.store(true, Relaxed);
                LAST_BUSY.store(now_s(), Relaxed);
                openfader::stems::warm(p.extensions.stems_multithread);
            }
        });
    }
}

impl Deck {
    /// Starts allocation and separation off the UI thread; unseparated frames play the original.
    pub(in crate::app::deck) fn stems_start(&self, file: PathBuf, pcm: Arc<audio::Track16>) {
        let (stems, fast, multithread, from_file) = prefs::with(|p| {
            (
                p.stems,
                p.stems_fast,
                p.extensions.stems_multithread,
                p.extensions.stem_files,
            )
        });
        if !stems {
            return;
        }
        if self.stems.borrow().job.is_some() || self.stems.borrow().ready {
            return;
        }
        self.stemwave_pcm(&pcm);
        let quality = if fast {
            Quality::Speed
        } else {
            Quality::Quality
        };
        let parts = from_file.then(|| stemfile::read_info(&file)).flatten();
        let job = match parts.clone() {
            Some(info) => openfader::stems::Job::from_stem_file(file, pcm, info),
            None => openfader::stems::Job::start(file, pcm, quality, multithread),
        };
        let mut s = self.stems.borrow_mut();
        s.complete = false;
        s.focus = None;
        s.parts = parts;
        s.text = progress_text(Phase::Opening, 0.0);
        s.job = Some(job);
        drop(s);
        self.show_stems();
    }

    pub(in crate::app::deck) fn completed_stems(&self) -> Option<Arc<audio::Stems>> {
        let s = self.stems.borrow();
        s.complete.then(|| s.attached.clone()).flatten()
    }

    pub(in crate::app::deck) fn stems_attach_completed(
        &self,
        file: PathBuf,
        pcm: Arc<audio::Track16>,
        stems: Arc<audio::Stems>,
    ) -> bool {
        let enabled = prefs::with(|p| p.stems);
        let same_file = self
            .loaded
            .borrow()
            .track
            .as_ref()
            .and_then(|t| t.file.as_ref())
            .is_some_and(|f| *f == file);
        let same_pcm = self
            .with_player(|p| Arc::ptr_eq(&p.pcm(), &pcm))
            .unwrap_or(false);
        let fast = prefs::with(|p| p.stems_fast);
        let compatible = compatible_stems(&stems, &pcm, fast);
        if !enabled || !same_file || !same_pcm || !compatible {
            return false;
        }
        if !self.attach_stems(&stems) {
            return false;
        }
        self.stemwave_pcm(&pcm);
        self.stemwave_stems(&stems);
        let mut s = self.stems.borrow_mut();
        s.job = None;
        s.complete = true;
        s.text = tr!("100% READY").into();
        drop(s);
        self.show_stems();
        true
    }

    fn attach_stems(&self, stems: &Arc<audio::Stems>) -> bool {
        match self.with_player(|p| p.set_stems(Some(stems.clone()))) {
            Some(Ok(())) => {
                let mut s = self.stems.borrow_mut();
                s.attached = Some(stems.clone());
                s.ready = true;
                drop(s);
                self.apply_stems();
                true
            }
            failed => {
                self.say(tr!(
                    "stems: {}",
                    failed
                        .and_then(Result::err)
                        .unwrap_or_else(|| tr!("no audio").into())
                ));
                false
            }
        }
    }

    /// SPEED separates the play position and the HOT CUEs first: tells the job where they are, when the play position
    /// moved to another segment or a cue was added or removed. The cue list is only collected when that key changed.
    fn stems_focus(&self) {
        let Some(dur) = self.with_player(|p| p.duration_ms()).filter(|&d| d > 0) else {
            return;
        };
        let pos = self.with_player(|p| p.position_ms()).unwrap_or(0.0);
        let loaded = self.loaded.borrow();
        let hot = || loaded.analysis.cues.iter().filter(|c| c.hot_cue > 0);
        let mut s = self.stems.borrow_mut();
        let Some(job) = &s.job else { return };
        let Some(stems) = job.stems() else { return };
        let frames = stems.frames();
        let at = |ms: f64| {
            (((ms / f64::from(dur)).clamp(0.0, 1.0) * frames as f64) as usize)
                .min(frames.saturating_sub(1))
        };
        let play = at(pos);
        let key = (play / stems.grid().stride(), hot().count());
        if s.focus != Some(key) {
            let cues: Vec<usize> = hot().map(|c| at(f64::from(c.time_ms))).collect();
            job.focus(play, &cues);
            s.focus = Some(key);
        }
    }

    /// Tick: shows the job's progress, keeps it pointed at the play position and takes its outcome.
    pub(in crate::app::deck) fn poll_stems(&self) {
        engines_tick(self.stems.borrow().job.is_some());
        let pending = {
            let s = self.stems.borrow();
            (!s.ready)
                .then(|| s.job.as_ref().and_then(|j| j.stems().cloned()))
                .flatten()
        };
        if let Some(stems) = pending {
            if !self.attach_stems(&stems) {
                self.stems_clear_job();
                return;
            }
            self.show_stems();
        }
        let result = {
            let mut s = self.stems.borrow_mut();
            let Some(job) = &s.job else { return };
            match job.poll() {
                None => {
                    let (phase, f) = job.progress.get();
                    let text = progress_text(phase, f);
                    let changed = text != s.text;
                    s.text = text;
                    drop(s);
                    self.stems_focus();
                    if changed {
                        self.show_stems();
                    }
                    return;
                }
                Some(r) => {
                    let stems = s.job.take().and_then(|j| j.stems().cloned());
                    r.map(|()| stems)
                }
            }
        };
        match result {
            Ok(stems) => {
                if let Some(stems) = &stems {
                    let ready = self.stems.borrow().ready;
                    if !ready && !self.attach_stems(stems) {
                        self.stems_clear_job();
                        return;
                    }
                }
                {
                    let mut s = self.stems.borrow_mut();
                    s.complete = true;
                    s.text = if s.parts.is_some() {
                        tr!("STEM FILE").into()
                    } else {
                        tr!("100% READY").into()
                    };
                }
                if let Some(stems) = stems {
                    self.stemwave_stems(&stems);
                }
                self.show_stems();
                self.finish_double();
            }
            Err(e) => {
                self.double.borrow_mut().take();
                self.with_player(|p| p.set_stems(None).ok());
                let mut s = self.stems.borrow_mut();
                s.attached = None;
                s.ready = false;
                s.text = if e.starts_with("STEMS model") {
                    tr!("NO MODEL").into()
                } else {
                    tr!("NO STEMS").into()
                };
                drop(s);
                self.show_stems();
                self.say(e);
            }
        }
    }

    /// The PREFS STEMS settings changed: switching on separates the track already loaded, switching off drops the stems.
    pub(in crate::app) fn stems_pref_changed(&self) {
        let now = prefs::with(|p| {
            (
                p.stems,
                p.stems_four,
                p.active_part_solo,
                p.extensions.stem_displays()[0],
                p.stems_fast,
                p.extensions.stem_files,
            )
        });
        let stems = now.0;
        let before = self.stems.borrow_mut().seen.replace(now);
        if before == Some(now) {
            return; // some other setting (the window size is saved on every resize)
        }
        if stems && before.is_some_and(|b| (b.4, b.5) != (now.4, now.5)) {
            // When the STEMS Analysis Process or the use of stem files changes the track gets its stems again from the
            // new source (a QUALITY cache entry answers SPEED at once).
            self.stems_clear_job();
        }
        if !stems {
            self.stems_clear_job();
            if self.pad_mode() == PadMode::Stems {
                self.set_pad_mode(0);
            }
        } else {
            self.ensure_stems(); // the job opens the engines; without a track they open on the first load
        }
        self.apply_stems();
        self.show_stems();
    }

    /// Starts or retries separation for the loaded track without restarting an attached job.
    pub(super) fn ensure_stems(&self) {
        let s = self.stems.borrow();
        if s.ready || s.job.is_some() {
            return;
        }
        drop(s);
        let file = self
            .loaded
            .borrow()
            .track
            .as_ref()
            .and_then(|t| t.file.clone());
        let pcm = self.with_player(|pl| pl.pcm()).filter(|c| c.frames() > 0);
        if let (Some(file), Some(pcm)) = (file, pcm) {
            self.stems_start(file, pcm);
        }
    }

    /// STEMS switched off: no job, no stems in the player (the mute state stays for when it comes back).
    fn stems_clear_job(&self) {
        let mut s = self.stems.borrow_mut();
        s.job = None;
        s.attached = None;
        s.parts = None;
        (s.ready, s.complete) = (false, false);
        s.text.clear();
        drop(s);
        self.with_player(|p| p.set_stems(None).ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_stems_require_completion_rate_length_and_sufficient_quality() {
        let pcm = audio::Track16::from(audio::Pcm {
            rate: 48_000,
            samples: vec![0.1; 32],
        });
        for quality in [Quality::Quality, Quality::Speed] {
            let stems = audio::Stems::new(
                pcm.frames(),
                openfader::stems::separate::grid(pcm.rate, quality),
            );
            assert!(!compatible_stems(&stems, &pcm, true));
            stems.finish();
            assert!(compatible_stems(&stems, &pcm, true));
            assert_eq!(
                compatible_stems(&stems, &pcm, false),
                quality == Quality::Quality
            );
        }
        let wrong_rate = audio::Stems::new(
            pcm.frames(),
            openfader::stems::separate::grid(44_100, Quality::Quality),
        );
        wrong_rate.finish();
        assert!(!compatible_stems(&wrong_rate, &pcm, true));
        let wrong_length = audio::Stems::new(
            pcm.frames() - 1,
            openfader::stems::separate::grid(pcm.rate, Quality::Quality),
        );
        wrong_length.finish();
        assert!(!compatible_stems(&wrong_length, &pcm, true));
    }
}
