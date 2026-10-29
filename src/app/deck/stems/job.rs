//! The separation job of a deck: start, focus, progress polling and the PREFS reaction.

use super::super::{Deck, PadMode};
use super::shared::{Source, Subscription, Version};
use crate::app::prefs;
use openfader::audio::{self, stemfile};
use openfader::stems::{Phase, Quality};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, LazyLock};

struct Prepared {
    parts: Option<stemfile::Info>,
    source: Option<Source>,
    identified: bool,
}

struct LookupWork {
    file: PathBuf,
    from_file: bool,
    cancelled: Arc<AtomicBool>,
    back: mpsc::Sender<Prepared>,
    pcm: Option<Arc<audio::Track16>>,
}

fn prepare(
    cancelled: &AtomicBool,
    parts: impl FnOnce() -> Option<stemfile::Info>,
) -> Option<Prepared> {
    if cancelled.load(Relaxed) {
        return None;
    }
    let parts = parts();
    (!cancelled.load(Relaxed)).then_some(Prepared {
        parts,
        source: None,
        identified: false,
    })
}

static LOOKUPS: LazyLock<(Mutex<VecDeque<LookupWork>>, Condvar)> = LazyLock::new(|| {
    for _ in 0..2 {
        std::thread::spawn(|| {
            loop {
                let work = {
                    let (queue, ready) = &*LOOKUPS;
                    let mut queue = queue.lock();
                    while queue.is_empty() {
                        ready.wait(&mut queue);
                    }
                    queue.pop_front().expect("not empty")
                };
                let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let before = work.pcm.as_ref().and_then(|_| Version::at(&work.file));
                    let mut prepared = prepare(&work.cancelled, || {
                        work.from_file
                            .then(|| stemfile::read_info(&work.file))
                            .flatten()
                    })?;
                    if let Some(pcm) = &work.pcm {
                        prepared.source = Source::identify(
                            &work.file,
                            pcm,
                            prepared.parts.is_some(),
                            before,
                            &|| work.cancelled.load(Relaxed),
                        );
                        prepared.identified = true;
                    }
                    (!work.cancelled.load(Relaxed)).then_some(prepared)
                }))
                .ok()
                .flatten();
                if let Some(prepared) = prepared {
                    let _ = work.back.send(prepared);
                }
            }
        });
    }
    (Mutex::new(VecDeque::new()), Condvar::new())
});

pub(super) struct Lookup {
    file: PathBuf,
    quality: Quality,
    multithread: bool,
    from_file: bool,
    cancelled: Arc<AtomicBool>,
    rx: Receiver<Prepared>,
    ready: Option<Prepared>,
    pcm: Option<Arc<audio::Track16>>,
}

impl Lookup {
    fn new(file: PathBuf, quality: Quality, multithread: bool, from_file: bool) -> Self {
        let (back, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut queue = LOOKUPS.0.lock();
        queue.retain(|w| !w.cancelled.load(Relaxed));
        // Four decks retain one request each; active filesystem calls are bounded by the two workers.
        if queue.len() < 8 {
            queue.push_back(LookupWork {
                file: file.clone(),
                from_file,
                cancelled: cancelled.clone(),
                back,
                pcm: None,
            });
            LOOKUPS.1.notify_one();
        }
        Self {
            file,
            quality,
            multithread,
            from_file,
            cancelled,
            rx,
            ready: None,
            pcm: None,
        }
    }

    fn identify(&mut self) {
        let (back, rx) = mpsc::channel();
        self.rx = rx;
        let mut queue = LOOKUPS.0.lock();
        queue.retain(|w| !w.cancelled.load(Relaxed));
        if queue.len() < 8 {
            queue.push_back(LookupWork {
                file: self.file.clone(),
                from_file: self.from_file,
                cancelled: self.cancelled.clone(),
                back,
                pcm: self.pcm.clone(),
            });
            LOOKUPS.1.notify_one();
        }
    }
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.cancelled.store(true, Relaxed);
        LOOKUPS
            .0
            .lock()
            .retain(|w| !Arc::ptr_eq(&w.cancelled, &self.cancelled));
    }
}

fn progress_text(phase: Phase, f: f32) -> String {
    let pct = (f * 100.0) as u32;
    match phase {
        Phase::Opening => tr!("STEMS 0%").into(),
        Phase::Waiting => tr!("RAM WAIT").into(),
        Phase::Download => tr!("MODEL {}%", pct),
        Phase::Separate => tr!("STEMS {}%", pct),
    }
}

fn cue_key(cues: impl Iterator<Item = usize>) -> u64 {
    let mut hash = DefaultHasher::new();
    for cue in cues {
        cue.hash(&mut hash);
    }
    hash.finish()
}

fn compatible_stems(stems: &audio::Stems, pcm: &audio::Track16, fast: bool) -> bool {
    let quality = openfader::stems::separate::grid(pcm.rate, Quality::Quality);
    let speed = openfader::stems::separate::grid(pcm.rate, Quality::Speed);
    stems.is_complete()
        && stems.frames() == pcm.frames()
        && (stems.grid() == quality || (fast && stems.grid() == speed))
}

impl Deck {
    /// A track starts decoding: probe stem-file metadata off the UI thread; models open only for uncached inference.
    pub(in crate::app::deck) fn stems_prepare(&self, file: Option<&std::path::Path>) {
        let (enabled, fast, multithread, from_file) = prefs::with(|p| {
            (
                p.stems,
                p.stems_fast,
                p.extensions.stems_multithread,
                p.extensions.stem_files,
            )
        });
        if enabled && let Some(file) = file {
            let quality = if fast {
                Quality::Speed
            } else {
                Quality::Quality
            };
            self.stems.borrow_mut().lookup = Some(Lookup::new(
                file.to_owned(),
                quality,
                multithread,
                from_file,
            ));
        }
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
        let mut s = self.stems.borrow_mut();
        let same_lookup = s.lookup.as_ref().is_some_and(|lookup| {
            lookup.file == file
                && lookup.quality == quality
                && lookup.multithread == multithread
                && lookup.from_file == from_file
        });
        if !same_lookup {
            s.lookup = None;
            s.lookup = Some(Lookup::new(file, quality, multithread, from_file));
        }
        s.lookup.as_mut().expect("installed").pcm = Some(pcm);
        s.complete = false;
        s.focus = None;
        s.text = progress_text(Phase::Opening, 0.0);
        drop(s);
        self.show_stems();
    }

    fn poll_stems_lookup(&self) {
        let Some(mut lookup) = self.stems.borrow_mut().lookup.take() else {
            return;
        };
        if lookup.ready.is_some() && lookup.pcm.is_none() {
            self.stems.borrow_mut().lookup = Some(lookup);
            return;
        }
        let prepared = match lookup.ready.take() {
            Some(ready) => ready,
            None => match lookup.rx.try_recv() {
                Ok(ready) => ready,
                Err(TryRecvError::Empty) => {
                    self.stems.borrow_mut().lookup = Some(lookup);
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    self.stems.borrow_mut().text = tr!("NO STEMS").into();
                    self.show_stems();
                    return;
                }
            },
        };
        let current = prefs::with(|p| {
            p.stems
                && p.stems_fast == (lookup.quality == Quality::Speed)
                && p.extensions.stem_files == lookup.from_file
                && p.extensions.stems_multithread == lookup.multithread
        });
        let same_file = self
            .loaded
            .borrow()
            .track
            .as_ref()
            .and_then(|t| t.file.as_ref())
            .is_some_and(|f| *f == lookup.file);
        if !current || !same_file {
            return;
        }
        if lookup.pcm.is_some() && !prepared.identified {
            lookup.identify();
            self.stems.borrow_mut().lookup = Some(lookup);
            return;
        }
        if let Some(pcm) = lookup.pcm.take() {
            let job = Subscription::start(
                lookup.file.clone(),
                pcm,
                lookup.quality,
                lookup.multithread,
                lookup.from_file,
                prepared.parts.clone(),
                prepared.source,
            );
            let mut s = self.stems.borrow_mut();
            s.parts = prepared.parts;
            s.job = Some(job);
            drop(s);
            self.show_stems();
        } else {
            lookup.ready = Some(prepared);
            self.stems.borrow_mut().lookup = Some(lookup);
        }
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
        s.lookup = None;
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
    /// moved to another segment or a cue was edited. The cue list is only collected when that key changed.
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
        let key = (
            play / stems.grid().stride(),
            cue_key(hot().map(|c| at(f64::from(c.time_ms)))),
        );
        if s.focus != Some(key) {
            let cues: Vec<usize> = hot().map(|c| at(f64::from(c.time_ms))).collect();
            job.focus(play, &cues);
            s.focus = Some(key);
        }
    }

    /// Tick: shows the job's progress, keeps it pointed at the play position and takes its outcome.
    pub(in crate::app::deck) fn poll_stems(&self) {
        self.poll_stems_lookup();
        let pending = {
            let s = self.stems.borrow();
            (!s.ready)
                .then(|| s.job.as_ref().and_then(|j| j.stems()))
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
                    let (phase, f) = job.progress();
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
                    let stems = s.job.take().and_then(|j| j.stems());
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
        s.lookup = None;
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
    fn cancelled_blocked_preflight_cannot_publish_metadata() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (_back, rx) = mpsc::channel();
        let owner = Lookup {
            file: PathBuf::from("old.wav"),
            quality: Quality::Quality,
            multithread: false,
            from_file: true,
            cancelled: cancelled.clone(),
            rx,
            ready: None,
            pcm: None,
        };
        let (started, begin) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            prepare(&cancelled, || {
                started.send(()).unwrap();
                wait.recv().unwrap();
                None
            })
        });
        begin
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        drop(owner);
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_none());
    }

    #[test]
    fn preflight_without_a_stem_file_needs_no_model_or_cache_probe() {
        let prepared = prepare(&AtomicBool::new(false), || None).unwrap();
        assert!(prepared.parts.is_none());
    }

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

    #[test]
    fn cue_focus_changes_when_a_cue_moves_without_changing_the_count() {
        assert_ne!(
            cue_key([100, 200].into_iter()),
            cue_key([100, 300].into_iter())
        );
        assert_ne!(cue_key([100, 200].into_iter()), cue_key([100].into_iter()));
        assert_eq!(
            cue_key([100, 200].into_iter()),
            cue_key([100, 200].into_iter())
        );
    }
}
