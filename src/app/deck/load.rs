//! LOAD: the analysis is read at once (waveforms show immediately), the audio decodes on a worker
//! thread and is picked up by [`Deck::poll_decode`] from the deck tick. Tracks the library has no
//! beat grid or 3-band waveform for are analysed on that same worker ([`analysis::analyze`]) and
//! those parts are filled in on arrival (session only, never written back).
//! A new LOAD replaces the whole [`Loaded`], dropping the old receiver: a stale decode is never
//! delivered. While decoding, the controls stay disabled (`DeckView.loaded` is false).
//! With STEMS and the STEM waveform on, [`stemwave`](super::stemwave) swaps in the waves of the mix heard once the stems are ready.

use super::{
    Deck, Decoded, Loaded, Pending, at_end, autocue, bpm_text, cuelist, fmt_ms, gain, grid_bpm,
    render, tempo_text,
};
use crate::app::prefs;
use openfader::{
    analysis, audio,
    library::{Library, Track},
    plugin::{event, source},
};
use serde_json::json;
use slint::{ModelRc, VecModel};
use std::{
    cell::RefCell,
    sync::mpsc::{Receiver, TryRecvError, channel},
};

/// Decode `path`; when `need` (no grid, no 3-band waveform) is set, also analyse the audio before
/// sending, so the PCM is never cloned.
fn decode_and_analyze(path: std::path::PathBuf, need: bool) -> Receiver<Result<Decoded, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(audio::decode(&path).map(|pcm| {
            let a = need.then(|| analysis::cache::analyze_cached(&path, &pcm.samples, pcm.rate).0);
            (pcm, a)
        }));
    });
    rx
}

/// A track on its way from a plugin source: the worker's answer (the downloaded file) and the track it is for.
pub(super) struct Download {
    rx: Receiver<Result<std::path::PathBuf, String>>,
    track: Track,
}

thread_local! {
    /// How to find the track after another in the list the browser shows (CONTINUOUS PLAYING).
    static NEXT: RefCell<Option<Box<dyn Fn(&Track) -> Option<Track>>>> = const { RefCell::new(None) };
}

/// The browser tells how to find the track after `t` in the list it shows: what CONTINUOUS PLAYING loads
/// when a track ends (`None` at the end of the list or when `t` is not in it).
pub fn set_next_track(f: impl Fn(&Track) -> Option<Track> + 'static) {
    NEXT.with(|n| *n.borrow_mut() = Some(Box::new(f)));
}

impl Deck {
    pub fn load(&self, t: &Track, lib: &dyn Library) {
        if self.eject_locked() {
            return;
        }
        self.bank.borrow_mut().take(); // an ordinary LOAD drops a Cue Bank ([`Deck::bank_pad`] puts it back)
        self.autoplay.set(false);
        self.download.borrow_mut().take(); // a download in progress is for a track that is no longer wanted
        let downloading = t.file.is_none() && t.remote.is_some();
        let mut notes = Vec::new();
        crate::app::device::track_loading(t); // cue sync: the copy loaded wins over its other copies, before it is read
        let a = match lib.analysis(t) {
            Some(a) => a,
            None => {
                notes.push(
                    if downloading {
                        "downloading…"
                    } else {
                        "not analysed"
                    }
                    .into(),
                );
                analysis::Analysis::default()
            }
        };
        self.install(t, a, notes, None);
        if let Some(remote) = t.remote.clone().filter(|_| downloading) {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(source::fetch(&remote));
            });
            *self.download.borrow_mut() = Some(Download {
                rx,
                track: t.clone(),
            });
        }
    }

    /// Picks up a finished download: the track loads again, now with its file. A failure ends the loading state.
    fn poll_download(&self) {
        let answer = match self.download.borrow().as_ref().map(|d| d.rx.try_recv()) {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(r)) => r,
            Some(Err(TryRecvError::Disconnected)) => Err("the download stopped".into()),
        };
        let Some(Download { track, .. }) = self.download.borrow_mut().take() else {
            return;
        };
        match answer {
            Ok(file) => {
                // CONTINUOUS PLAYING asked for autoplay before the file was there. The serial stays what the first stage made
                // it: AUTOMIX and the on-air watchers know this LOAD by it.
                let (autoplay, serial) = (self.autoplay.get(), self.load_serial.get());
                self.load(
                    &Track {
                        file: Some(file),
                        ..track
                    },
                    &*self.lib,
                );
                self.autoplay.set(autoplay);
                self.load_serial.set(serial);
            }
            Err(e) => self.show(|v| {
                (v.loading, v.loaded) = (false, true);
                v.status = format!("download failed: {e}").into();
            }),
        }
    }

    /// Puts `t` (analysis `a`) on the deck. The audio is `pcm` when the caller already has it (the sampler's
    /// slice for S.SCRATCH, picked up at once by [`Deck::poll_decode`]), else decoded from `t.file` on a worker.
    pub(super) fn install(
        &self,
        t: &Track,
        mut a: analysis::Analysis,
        notes: Vec<String>,
        pcm: Option<audio::Pcm>,
    ) {
        // LOAD SETTING: the track's own cue nearest the start wins over the others (before the automatic cues are added).
        let nearest = prefs::with(|p| p.controller.load_nearest_cue)
            .then(|| autocue::nearest(&a.cues))
            .flatten();
        analysis::auto::fill(&mut a); // phrases from the grid + 3-band waveform when the library has none
        if a.cues.is_empty() {
            let auto = analysis::auto::cues(&a); // session-only automatic cues (empty without a grid or when cues exist)
            a.cues.extend(auto);
        }
        let duration = if t.length_s > 0 {
            t.length_s as u32 * 1000
        } else {
            a.beats.last().map_or(0, |b| b.time_ms)
        };
        let cue_ms = nearest.unwrap_or_else(|| {
            a.cues
                .iter()
                .find(|c| c.hot_cue == 0)
                .map_or(0, |c| c.time_ms)
        });
        let grid = grid_bpm(&a.beats);
        let mut notes = notes;
        if self.player.is_some() && t.file.is_none() && t.remote.is_none() && pcm.is_none() {
            notes.push("no file path".into());
        }
        let mut status = summary(&t.title, &a, grid, &notes);
        let library = pcm.is_none(); // a sampler slice is no library track: not exported, not in the history
        let downloading = library && t.file.is_none() && t.remote.is_some();
        self.rate.set(1.0);
        self.key_st.set(0);
        self.sync_reset();
        self.cue_state.set(Default::default());
        self.pending.set(None);
        self.gate.set(None);
        self.plugin_held.set(None);
        self.jog_cue.set(false);
        self.stems_clear();
        let mut pending = None;
        if let Some(player) = &self.player {
            let mut player = player.borrow_mut();
            player.load(audio::Pcm::default());
            player.set_rate(1.0);
            player.set_key_shift(0.0);
            player.seek(cue_ms);
            // Analyse only what is missing; analyze() fills both parts, poll_decode keeps whichever the library had.
            let need = a.beats.is_empty() || a.three_band_detail.is_empty();
            if need && t.file.is_some() && pcm.is_none() {
                status += " · analyzing…";
            }
            let rx = match pcm {
                Some(pcm) => {
                    let (tx, rx) = channel();
                    let _ = tx.send(Ok((pcm, None)));
                    Some(rx)
                }
                None => t.file.as_ref().map(|f| decode_and_analyze(f.clone(), need)),
            };
            pending = rx.map(|rx| Pending {
                rx,
                title: t.title.clone(),
                notes,
            });
        }
        if pending.is_some() {
            self.stems_warm(t.file.as_deref());
        }
        let loading = pending.is_some() || downloading;
        let cue_views = cuelist::views(&a);
        self.load_serial.set(self.load_serial.get().wrapping_add(1));
        let slip = self.with_player(|p| p.slip()).unwrap_or(false); // slip mode persists across loads
        let base_bpm = grid.unwrap_or(t.bpm_x100 as f64 / 100.0);
        *self.loaded.borrow_mut() = Loaded {
            ready: true,
            pending,
            detail: render::detail::wave(&a, None),
            overview: render::overview::wave(&a, duration),
            lib_beats: a.beats.clone(),
            analysis: a,
            duration_ms: duration,
            cue_ms,
            cue_locked: nearest.is_some(),
            base_bpm,
            key: t.key.clone(),
            lib_bpm: t.bpm_x100 as f64 / 100.0,
            track: library.then(|| t.clone()),
            ..Default::default()
        };
        let pad_views = self.pad_views(&self.loaded.borrow());
        self.show(|v| {
            v.loaded = !loading;
            v.loading = loading;
            v.pads = ModelRc::new(VecModel::from(pad_views));
            v.title = t.title.clone().into();
            v.artist = t.artist.clone().into();
            (v.elapsed_text, v.remain_text) =
                (fmt_ms(0).into(), format!("-{}", fmt_ms(duration)).into());
            v.status = if loading {
                format!("loading… · {status}")
            } else {
                status
            }
            .into();
            v.has_grid = grid.is_some();
            v.bpm = base_bpm as f32;
            v.cues = ModelRc::new(VecModel::from(cue_views));
            v.tempo = 0.5;
            v.bpm_text = bpm_text(grid, 1.0);
            v.tempo_text = tempo_text(1.0);
            v.reverse = false;
            v.slip = slip;
            v.ending = false;
            v.loop_beats = 0;
            v.loop_active = false;
            v.grid_edit = false;
        });
        self.refresh_key();
        if library && !downloading {
            event::fire(
                "track_loaded",
                json!({"deck": self.index + 1, "track": source::track_json(t)}),
            );
        }
    }

    /// The waveform colour setting changed: rebuild both waveforms from the analysis (a detail wave
    /// made from the audio, for a track the analysis has none for, stays). While the STEM waveform is
    /// shown the rebuilt originals are parked behind it ([`stemwave`](super::stemwave)). The tick redraws.
    pub(in crate::app) fn rewave(&self) {
        let mut l = self.loaded.borrow_mut();
        let l = &mut *l;
        let (detail, overview) = (
            render::detail::wave(&l.analysis, None),
            render::overview::wave(&l.analysis, l.duration_ms),
        );
        let mut sw = self.stemwave.borrow_mut();
        let (d, o) = match sw.orig.as_mut() {
            Some((d, o)) => (d, o),
            None => (&mut l.detail, &mut l.overview),
        };
        if detail.is_some() {
            *d = detail;
        }
        *o = overview;
        drop(sw);
        l.invalidate();
    }

    /// The loaded track with this session's grid and cues (hot / memory cues and saved loops),
    /// for Export XML; `None` before the first LOAD.
    pub(in crate::app) fn session(
        &self,
    ) -> Option<(Track, Vec<analysis::Beat>, Vec<analysis::Cue>)> {
        let l = self.loaded.borrow();
        l.track
            .clone()
            .map(|t| (t, l.analysis.beats.clone(), l.analysis.cues.clone()))
    }

    /// Picks up a finished decode: hands the audio to the player, falls back to audio peaks for
    /// the detail waveform when the analysis has none, and enables the controls.
    pub(super) fn poll_decode(&self) {
        self.poll_download();
        let mut l = self.loaded.borrow_mut();
        let Some(p) = &l.pending else { return };
        let res = match p.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(r) => r,
            Err(TryRecvError::Disconnected) => Err("decoder stopped".into()),
        };
        let (title, mut notes) = l
            .pending
            .take()
            .map_or_else(Default::default, |p| (p.title, p.notes));
        let mut added_cues = false;
        let mut stem_src = None;
        match res {
            Ok((pcm, analysed)) => {
                // Fill only what the library lacked; keep its beats / waveform otherwise.
                if let Some(an) = analysed {
                    notes.retain(|n| n != "not analysed");
                    l.analysis.key = an.key.clone();
                    if l.analysis.beats.is_empty() && !an.beats.is_empty() {
                        l.lib_beats = an.beats.clone();
                        l.analysis.beats = an.beats;
                        notes.push("grid: analysed".into());
                    }
                    if l.analysis.three_band_detail.is_empty() && !an.three_band_detail.is_empty() {
                        l.analysis.three_band_detail = an.three_band_detail;
                        l.analysis.three_band_preview = an.three_band_preview;
                        l.detail = render::detail::wave(&l.analysis, Some(&pcm));
                        notes.push("waveform: analysed".into());
                    }
                }
                // The analysis may have just given this track a grid and waveform: phrases and automatic cues follow.
                analysis::auto::fill(&mut l.analysis);
                if l.analysis.cues.is_empty() {
                    let auto = analysis::auto::cues(&l.analysis);
                    added_cues = !auto.is_empty();
                    l.analysis.cues.extend(auto);
                }
                if l.detail.is_none() {
                    l.detail = render::detail::wave(&l.analysis, Some(&pcm));
                    notes.push("detail: from audio".into());
                }
                // AUTO GAIN comes from the store (measured and saved on the track's first load, [`gain::resolve`]); AUTO CUE
                // puts the cue point (and the playhead) on the first sound unless LOAD SETTING chose a cue. A sampler slice is no track to cue.
                (l.gain_analysed, l.gain_adjust) = gain::resolve(l.track.as_ref(), &pcm);
                let (auto_cue, db) =
                    prefs::with(|p| (p.controller.auto_cue, p.controller.auto_cue_db()));
                if auto_cue && !l.cue_locked && l.track.is_some() {
                    if let Some(ms) = autocue::first_sound(&pcm, db) {
                        l.cue_ms = ms;
                    }
                }
                // STEMS start after the player has the audio.
                stem_src = l
                    .track
                    .as_ref()
                    .and_then(|t| t.file.clone())
                    .filter(|_| prefs::with(|p| p.stems));
                if let Some(player) = &self.player {
                    let mut player = player.borrow_mut();
                    player.load(pcm);
                    player.seek(l.cue_ms);
                    l.duration_ms = player.duration_ms();
                }
                l.overview = render::overview::wave(&l.analysis, l.duration_ms);
            }
            Err(e) => notes.push(format!("audio: {e}")),
        }
        l.invalidate();
        let grid = grid_bpm(&l.analysis.beats);
        let status = summary(&title, &l.analysis, grid, &notes);
        if let Some(g) = grid {
            l.base_bpm = g;
        }
        if l.key.is_empty() {
            l.key = l.analysis.key.clone().unwrap_or_default(); // library key wins, else the detected one
        }
        let (base, rate) = (l.base_bpm, self.rate.get());
        let (cue_views, pad_views) = if added_cues {
            (Some(cuelist::views(&l.analysis)), Some(self.pad_views(&l)))
        } else {
            (None, None)
        };
        drop(l);
        self.show(|v| {
            v.loaded = true;
            v.loading = false;
            v.status = status.into();
            v.has_grid = grid.is_some();
            if let (Some(c), Some(p)) = (cue_views, pad_views) {
                v.cues = ModelRc::new(VecModel::from(c));
                v.pads = ModelRc::new(VecModel::from(p));
            }
            v.bpm_text = bpm_text(grid, rate);
            v.bpm = (base * rate) as f32;
        });
        self.refresh_key();
        self.push_beats(); // the player's metronome clicks on the final grid (Player::load cleared it)
        if self.autoplay.take() {
            self.with_player(|p| p.play());
        }
        // The player's track itself, shared: no copy of the audio for the separation.
        if let Some((file, pcm)) = stem_src.zip(self.with_player(|p| p.pcm())) {
            self.stems_start(file, pcm);
        }
    }

    /// EJECT/LOAD LOCK: a LOAD onto a playing deck is refused.
    pub(super) fn eject_locked(&self) -> bool {
        let locked = prefs::with(|p| p.controller.eject_lock) && self.playing();
        if locked {
            self.say("eject lock: pause the deck before loading another track");
        }
        locked
    }

    /// The deck stopped (see `follow`): with CONTINUOUS PLAYING, a track that ran to its end is followed by the
    /// next one of the browser's list, started as soon as it has decoded.
    pub(super) fn track_ended(&self) {
        if !prefs::with(|p| p.controller.continuous_play) {
            return;
        }
        let (pos, track, duration) = {
            let l = self.loaded.borrow();
            (self.clock(&l).0, l.track.clone(), l.duration_ms)
        };
        let Some(track) = track.filter(|_| at_end(pos, duration)) else {
            return;
        };
        match NEXT.with(|n| n.borrow().as_ref().and_then(|next| next(&track))) {
            Some(next) => {
                self.load(&next, &*self.lib);
                self.autoplay.set(true);
            }
            None => self.say("continuous playing: no next track in the list"),
        }
    }
}

/// The deck status line: `<title> — N beats @ BPM, N cues · notes…`, rebuilt when the analysis arrives.
fn summary(title: &str, a: &analysis::Analysis, grid: Option<f64>, notes: &[String]) -> String {
    std::iter::once(format!(
        "{title} — {} beats @ {}, {} cues",
        a.beats.len(),
        bpm_text(grid, 1.0),
        a.cues.len()
    ))
    .chain(notes.iter().cloned())
    .collect::<Vec<_>>()
    .join(" · ")
}
