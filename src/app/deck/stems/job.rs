//! The separation job of a deck: start, focus, progress polling and the PREFS reaction.

use super::super::{Deck, PadMode};
use crate::app::prefs;
use openfader::audio;
use openfader::stems::{Phase, Quality};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};
use std::{path::PathBuf, time::Instant};

fn progress_text(phase: Phase, f: f32) -> String {
    let pct = (f * 100.0) as u32;
    match phase {
        Phase::Opening => "STEMS 0%".into(),
        Phase::Download => format!("MODEL {pct}%"),
        Phase::Separate => format!("STEMS {pct}%"),
    }
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
            // A cached track separates nothing: the job reads the entry, so no engine is needed.
            if p.stems && !file.is_some_and(|f| openfader::stems::cache::has(f, quality)) {
                WARM.store(true, Relaxed);
                LAST_BUSY.store(now_s(), Relaxed);
                openfader::stems::warm(p.extensions.stems_multithread);
            }
        });
    }
}

impl Deck {
    /// Starts separating `pcm` (the decoded `file`), when STEMS is on: the stems go to the player at once (frames not
    /// separated yet play as the original track) and the buttons work from now on.
    pub(in crate::app::deck) fn stems_start(&self, file: PathBuf, pcm: Arc<audio::Track16>) {
        let (stems, fast, multithread) =
            prefs::with(|p| (p.stems, p.stems_fast, p.extensions.stems_multithread));
        if !stems {
            return;
        }
        self.stemwave_pcm(&pcm);
        let quality = if fast {
            Quality::Speed
        } else {
            Quality::Quality
        };
        let job = openfader::stems::Job::start(file, pcm, quality, multithread);
        let attached = self.with_player(|pl| pl.set_stems(Some(job.stems().clone())));
        let mut s = self.stems.borrow_mut();
        s.complete = false;
        s.focus = None;
        match attached {
            Some(Ok(())) => {
                s.ready = true;
                s.text = progress_text(Phase::Opening, 0.0);
                s.job = Some(job);
            }
            failed => {
                s.text = "NO STEMS".into();
                drop(s);
                self.say(format!(
                    "stems: {}",
                    failed
                        .and_then(Result::err)
                        .unwrap_or_else(|| "no audio".into())
                ));
                return self.show_stems();
            }
        }
        drop(s);
        self.apply_stems();
        self.show_stems();
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
        let frames = job.stems().frames();
        let at = |ms: f64| {
            (((ms / f64::from(dur)).clamp(0.0, 1.0) * frames as f64) as usize)
                .min(frames.saturating_sub(1))
        };
        let play = at(pos);
        let key = (play / job.stems().grid().stride(), hot().count());
        if s.focus != Some(key) {
            let cues: Vec<usize> = hot().map(|c| at(f64::from(c.time_ms))).collect();
            job.focus(play, &cues);
            s.focus = Some(key);
        }
    }

    /// Tick: shows the job's progress, keeps it pointed at the play position and takes its outcome.
    pub(in crate::app::deck) fn poll_stems(&self) {
        engines_tick(self.stems.borrow().job.is_some());
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
                    let stems = s.job.take().map(|j| j.stems().clone());
                    r.map(|()| stems)
                }
            }
        };
        match result {
            Ok(stems) => {
                {
                    let mut s = self.stems.borrow_mut();
                    (s.complete, s.text) = (true, "100% READY".into());
                }
                if let Some(stems) = stems {
                    self.stemwave_stems(&stems);
                }
                self.show_stems();
                self.finish_double();
            }
            Err(e) => {
                self.double.set(None);
                self.with_player(|p| p.set_stems(None).ok());
                let mut s = self.stems.borrow_mut();
                s.ready = false;
                s.text = if e.starts_with("STEMS model") {
                    "NO MODEL".into()
                } else {
                    "NO STEMS".into()
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
            )
        });
        let stems = now.0;
        let before = self.stems.borrow_mut().seen.replace(now);
        if before == Some(now) {
            return; // some other setting (the window size is saved on every resize)
        }
        if stems && before.is_some_and(|b| b.4 != now.4) {
            // When the STEMS Analysis Process changes the track is separated again at the new
            // setting (a QUALITY cache entry answers SPEED at once).
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
        (s.ready, s.complete) = (false, false);
        s.text.clear();
        drop(s);
        self.with_player(|p| p.set_stems(None).ok());
    }
}
