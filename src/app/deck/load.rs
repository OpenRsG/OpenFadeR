//! LOAD publishes decoded audio before optional analysis. The worker also prepares loudness,
//! auto cue and the audio-only waveform; the UI merges late results without replacing edits.
//! Replacing the loaded track cancels its worker at decode packet and analysis stage boundaries.

use super::{
    Deck, Loaded, Pending, at_end, autocue, bpm_text, cuelist, fmt_ms, gain, grid_bpm, render,
    tempo_text,
};
use crate::app::prefs::{self, analysis::GridSource};
use openfader::{
    analysis, audio,
    library::{Library, Track},
    plugin::{event, source},
    store,
};
use serde_json::json;
use slint::{ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError, channel},
    },
};

pub(super) enum Decoded {
    Playable {
        pcm: Arc<audio::Track16>,
        stems: Option<Arc<audio::Stems>>,
        gain: Option<f32>,
        cue: Option<u32>,
    },
    Detail(Option<render::Wave>),
    Analyzed(analysis::Analysis),
    Finished,
}

fn decode_and_analyze(
    t: Track,
    need: bool,
    pcm: Option<audio::Pcm>,
    library: bool,
    auto_cue: Option<f32>,
    fallback: bool,
    cancelled: Arc<AtomicBool>,
) -> Receiver<Result<Decoded, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let stop = || cancelled.load(Ordering::Relaxed);
        let pcm = match pcm.map(Ok).unwrap_or_else(|| {
            t.file
                .as_deref()
                .ok_or_else(|| "no file path".into())
                .and_then(|file| audio::decode_cancellable(file, &stop))
        }) {
            Ok(pcm) => pcm,
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        if stop() {
            return;
        }
        let gain = gain::resolve(library.then_some(&t), &pcm).0;
        if stop() {
            return;
        }
        let cue = auto_cue.and_then(|db| autocue::first_sound(&pcm, db));
        if stop()
            || tx
                .send(Ok(Decoded::Playable {
                    pcm: Arc::new(audio::Track16::from(&pcm)),
                    stems: None,
                    gain: Some(gain),
                    cue,
                }))
                .is_err()
        {
            return;
        }
        let detail = fallback.then(|| render::detail::audio_wave(&pcm)).flatten();
        if stop() || tx.send(Ok(Decoded::Detail(detail))).is_err() {
            return;
        }
        if library {
            analysis::cache::remember_fingerprint(&t, &pcm);
        }
        if need && !stop() {
            let Some((a, _)) =
                analysis::cache::analyze_cached_cancellable(&t, &pcm.samples, pcm.rate, &stop)
            else {
                return;
            };
            if stop() || tx.send(Ok(Decoded::Analyzed(a))).is_err() {
                return;
            }
        }
        let _ = tx.send(Ok(Decoded::Finished));
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
    /// Updates metadata without touching audio, analysis, transport or the load serial.
    pub(in crate::app) fn refresh_metadata(&self, edited: &Track) {
        let key = store::key(edited);
        let mut loaded = self.loaded.borrow_mut();
        if let Some(t) = loaded.track.as_mut().filter(|t| store::key(t) == key) {
            crate::app::trackinfo::copy_metadata(t, edited);
            self.show(|v| {
                v.title = t.title.as_str().into();
                v.artist = t.artist.as_str().into();
            });
            if let Some(pending) = loaded.pending.as_mut() {
                pending.title.clone_from(&edited.title);
            }
        }
        if let Some(d) = self.download.borrow_mut().as_mut()
            && store::key(&d.track) == key
        {
            crate::app::trackinfo::copy_metadata(&mut d.track, edited);
        }
    }

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
        let mut a = match lib.analysis(t) {
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
        let mut base = None;
        if t.file.is_some() && prefs::with(|p| p.analysis.grid_source) == GridSource::Ours {
            // Our grid replaces the library's; the user's own edits (grid, cues, phrases) stay on top of both.
            let ours = analysis::cache::load_for(t, &analysis::options::get())
                .ok()
                .flatten();
            analysis::cache::take_ours(&mut a, ours);
            base = Some(a.beats.clone());
            let edits = store::global().edits(&store::key(t)).unwrap_or_default();
            store::apply(&mut a, &edits);
        }
        self.install(t, a, notes, None);
        if let Some(b) = base {
            let mut l = self.loaded.borrow_mut();
            l.lib_beats.clone_from(&b);
            l.base_beats = Some(b);
        }
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
        a: analysis::Analysis,
        notes: Vec<String>,
        pcm: Option<audio::Pcm>,
    ) {
        self.install_audio(t, a, notes, pcm, None);
    }

    pub(super) fn install_shared(
        &self,
        t: &Track,
        a: analysis::Analysis,
        notes: Vec<String>,
        pcm: Arc<audio::Track16>,
        stems: Arc<audio::Stems>,
    ) {
        self.install_audio(t, a, notes, None, Some((pcm, stems)));
    }

    fn install_audio(
        &self,
        t: &Track,
        mut a: analysis::Analysis,
        notes: Vec<String>,
        pcm: Option<audio::Pcm>,
        shared: Option<(Arc<audio::Track16>, Arc<audio::Stems>)>,
    ) {
        // LOAD SETTING: the track's own cue nearest the start wins over the others (before the automatic cues are added).
        let nearest = prefs::with(|p| p.controller.load_nearest_cue)
            .then(|| autocue::nearest(&a.cues))
            .flatten();
        analysis::auto::fill(&mut a); // phrases from the grid + 3-band waveform when the library has none
        // session-only automatic cues (none without a grid or when the track has cues)
        let auto_cues = a.cues.is_empty() && {
            let auto = analysis::auto::cues(&a);
            a.cues.extend(auto);
            !a.cues.is_empty()
        };
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
        if self.player.is_some()
            && t.file.is_none()
            && t.remote.is_none()
            && pcm.is_none()
            && shared.is_none()
        {
            notes.push("no file path".into());
        }
        let status = summary(&t.title, &a, grid, auto_cues, &notes);
        let library = pcm.is_none(); // a sampler slice is no library track: not exported, not in the history
        let reused = shared.is_some();
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
        let detail = render::detail::wave(&a, None);
        if let Some(player) = &self.player {
            let mut player = player.borrow_mut();
            player.load(audio::Pcm::default());
            player.set_rate(1.0);
            player.set_key_shift(0.0);
            player.seek(cue_ms);
            let need = library && (a.beats.is_empty() || a.three_band_detail.is_empty());
            let cancelled = Arc::new(AtomicBool::new(false));
            let auto_cue = prefs::with(|p| {
                (library && nearest.is_none() && p.controller.auto_cue)
                    .then(|| p.controller.auto_cue_db())
            });
            let rx = match shared {
                Some((pcm, stems)) => {
                    let (tx, rx) = channel();
                    let _ = tx.send(Ok(Decoded::Playable {
                        pcm,
                        stems: Some(stems),
                        gain: None,
                        cue: None,
                    }));
                    let _ = tx.send(Ok(Decoded::Finished));
                    Some(rx)
                }
                None => (pcm.is_some() || t.file.is_some()).then(|| {
                    decode_and_analyze(
                        t.clone(),
                        need,
                        pcm,
                        library,
                        auto_cue,
                        detail.is_none(),
                        cancelled.clone(),
                    )
                }),
            };
            pending = rx.map(|rx| Pending {
                rx,
                cancelled,
                title: t.title.clone(),
                notes,
                playable: false,
                initial_cue: cue_ms,
            });
        }
        if pending.is_some() && !reused {
            self.stems_warm(t.file.as_deref());
        }
        let loading = pending.is_some() || downloading;
        let cue_views = cuelist::views(&a);
        self.load_serial.set(self.load_serial.get().wrapping_add(1));
        let slip = self.with_player(|p| p.slip()).unwrap_or(false); // slip mode persists across loads
        let base_bpm = grid.unwrap_or(t.bpm_x100 as f64 / 100.0);
        let (gain_analysed, gain_adjust) = gain::stored(library.then_some(t));
        *self.loaded.borrow_mut() = Loaded {
            ready: true,
            pending,
            detail,
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
            auto_cues: Cell::new(auto_cues),
            gain_analysed,
            gain_adjust,
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

    /// Installs audio as soon as it is decoded; optional results leave transport and edits intact.
    pub(super) fn poll_decode(&self) {
        self.poll_download();
        let mut l = self.loaded.borrow_mut();
        let Some(mut pending) = l.pending.take() else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Err(TryRecvError::Empty) => {
                l.pending = Some(pending);
                return;
            }
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => Err(if pending.playable {
                "analysis stopped"
            } else {
                "decoder stopped"
            }
            .into()),
        };
        let mut finished = false;
        let mut playable = false;
        let mut completed_stems = None;
        let mut added_cues = false;
        let mut detail_update = None;
        match result {
            Ok(Decoded::Playable {
                pcm,
                stems,
                gain,
                cue,
            }) => {
                completed_stems = stems;
                if let Some(gain) = gain {
                    l.gain_analysed = gain;
                }
                if !l.cue_locked && l.cue_ms == pending.initial_cue {
                    if let Some(ms) = cue {
                        l.cue_ms = ms;
                    }
                }
                if let Some(player) = &self.player {
                    let mut player = player.borrow_mut();
                    player.load_shared(pcm);
                    player.seek(l.cue_ms);
                    l.duration_ms = player.duration_ms();
                }
                pending.playable = true;
                playable = true;
            }
            Ok(Decoded::Detail(detail)) => {
                let missing = self
                    .stemwave
                    .borrow()
                    .orig
                    .as_ref()
                    .map_or(l.detail.is_none(), |(detail, _)| detail.is_none());
                if missing {
                    detail_update = detail;
                }
            }
            Ok(Decoded::Analyzed(an)) => {
                pending.notes.retain(|n| n != "not analysed");
                let edits = l
                    .track
                    .as_ref()
                    .and_then(|t| store::global().edits(&store::key(t)).ok())
                    .unwrap_or_default();
                merge_analysis(&mut l, an, &edits, &mut pending.notes);
                detail_update = render::detail::wave(&l.analysis, None);
                analysis::auto::fill(&mut l.analysis);
                let edited_cues = edits.cues.is_some();
                if l.analysis.cues.is_empty() && !edited_cues {
                    let cues = analysis::auto::cues(&l.analysis);
                    added_cues = !cues.is_empty();
                    l.auto_cues.set(added_cues);
                    l.analysis.cues.extend(cues);
                }
            }
            Ok(Decoded::Finished) => finished = true,
            Err(e) => {
                pending.notes.push(format!("audio: {e}"));
                finished = true;
            }
        }
        let overview = render::overview::wave(&l.analysis, l.duration_ms);
        {
            let mut sw = self.stemwave.borrow_mut();
            let (detail, over) = match sw.orig.as_mut() {
                Some((detail, overview)) => (detail, overview),
                None => {
                    let l = &mut *l;
                    (&mut l.detail, &mut l.overview)
                }
            };
            if let Some(update) = detail_update {
                *detail = Some(update);
            }
            *over = overview;
        }
        l.invalidate();
        let grid = grid_bpm(&l.analysis.beats);
        let status = summary(
            &pending.title,
            &l.analysis,
            grid,
            l.auto_cues.get(),
            &pending.notes,
        );
        if let Some(g) = grid {
            l.base_bpm = g;
        }
        if l.key.is_empty() {
            l.key = l.analysis.key.clone().unwrap_or_default();
        }
        let (base, rate) = (l.base_bpm, self.rate.get());
        let (cue_views, pad_views) = if added_cues {
            (Some(cuelist::views(&l.analysis)), Some(self.pad_views(&l)))
        } else {
            (None, None)
        };
        let ready = pending.playable || finished;
        let stem_src = playable
            .then(|| l.track.as_ref().and_then(|t| t.file.clone()))
            .flatten();
        if !finished {
            l.pending = Some(pending);
        }
        drop(l);
        self.show(|v| {
            v.loaded = ready;
            v.loading = !ready;
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
        self.push_beats();
        if playable && self.autoplay.take() {
            self.with_player(|p| p.play());
        }
        if let Some((file, pcm)) = stem_src.zip(self.with_player(|p| p.pcm())) {
            if completed_stems
                .is_some_and(|stems| self.stems_attach_completed(file.clone(), pcm.clone(), stems))
            {
                self.finish_double();
            } else {
                self.stems_start(file, pcm);
            }
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

fn merge_analysis(
    l: &mut Loaded,
    an: analysis::Analysis,
    edits: &store::Edits,
    notes: &mut Vec<String>,
) {
    if l.analysis.key.is_none() {
        l.analysis.key = an.key;
    }
    if l.analysis.beats.is_empty() && edits.grid.is_none() && !an.beats.is_empty() {
        l.lib_beats = an.beats.clone();
        l.analysis.beats = an.beats;
        notes.push("grid: analysed".into());
    }
    if l.analysis.three_band_detail.is_empty() && !an.three_band_detail.is_empty() {
        l.analysis.three_band_detail = an.three_band_detail;
        l.analysis.three_band_preview = an.three_band_preview;
        notes.push("waveform: analysed".into());
    }
}

/// The deck status line: `<title> — N beats @ BPM, N cues · notes…` (`N automatic cues` for this session's own),
/// rebuilt when the analysis arrives.
fn summary(
    title: &str,
    a: &analysis::Analysis,
    grid: Option<f64>,
    auto_cues: bool,
    notes: &[String],
) -> String {
    std::iter::once(format!(
        "{title} — {} beats @ {}, {} {}cues",
        a.beats.len(),
        bpm_text(grid, 1.0),
        a.cues.len(),
        if auto_cues { "automatic " } else { "" }
    ))
    .chain(notes.iter().cloned())
    .collect::<Vec<_>>()
    .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(time_ms: u32) -> analysis::Beat {
        analysis::Beat {
            time_ms,
            bpm_x100: 12000,
            bar_pos: 1,
        }
    }

    #[test]
    fn decoded_audio_is_readable_before_optional_analysis_and_keeps_initial_cue_gain() {
        let mut samples = vec![0.0; 800];
        samples.extend((0..7200).map(|i| (i as f32 * 0.17).sin() * 0.4));
        let pcm = audio::Pcm {
            rate: 4000,
            samples,
        };
        let expected = audio::Track16::from(&pcm);
        let expected_gain = audio::loudness::auto_gain_db(&pcm);
        let expected_cue = autocue::first_sound(&pcm, -36.0);
        let expected_detail = render::detail::audio_wave(&pcm).unwrap();
        let rx = decode_and_analyze(
            Track::default(),
            true,
            Some(pcm),
            false,
            Some(-36.0),
            true,
            Arc::new(AtomicBool::new(false)),
        );
        let Decoded::Playable { pcm, gain, cue, .. } = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap()
        else {
            panic!("audio must arrive before optional results")
        };
        let mut energy = 0.0;
        for frame in 0..pcm.frames() {
            let actual = pcm.frame_at(frame as f64).unwrap();
            assert_eq!(Some(actual), expected.frame_at(frame as f64));
            energy += actual[0] * actual[0];
        }
        assert!(
            energy > 100.0,
            "published audio is audible before analysis arrives"
        );
        assert_eq!(gain, Some(expected_gain));
        assert_eq!(cue, expected_cue);
        assert_eq!(cue, Some(100));
        let Decoded::Detail(Some(detail)) = rx.recv().unwrap().unwrap() else {
            panic!("audio-only waveform follows playable audio")
        };
        assert_eq!(detail.layers, expected_detail.layers);
        assert_eq!(detail.per_ms, expected_detail.per_ms);
        let Decoded::Analyzed(a) = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
        else {
            panic!("optional analysis follows playable audio")
        };
        assert!(!a.three_band_detail.is_empty());
        assert!(matches!(rx.recv().unwrap().unwrap(), Decoded::Finished));
    }

    #[test]
    fn late_analysis_fills_missing_waveform_without_replacing_library_or_edited_grid() {
        let detected = analysis::Analysis {
            beats: vec![beat(500)],
            three_band_detail: vec![[7, 8, 9]],
            three_band_preview: vec![[4, 5, 6]],
            key: Some("8A".into()),
            ..Default::default()
        };
        for original in [vec![beat(100)], vec![beat(200)]] {
            let mut loaded = Loaded {
                analysis: analysis::Analysis {
                    beats: original.clone(),
                    cues: vec![analysis::Cue {
                        time_ms: 300,
                        hot_cue: 0,
                        loop_end_ms: None,
                        color: None,
                        comment: None,
                    }],
                    key: Some("7A".into()),
                    ..Default::default()
                },
                lib_beats: vec![beat(50)],
                cue_ms: 300,
                gain_adjust: 2.5,
                ..Default::default()
            };
            merge_analysis(
                &mut loaded,
                detected.clone(),
                &store::Edits::default(),
                &mut Vec::new(),
            );
            assert_eq!(loaded.analysis.beats, original);
            assert_eq!(loaded.lib_beats, vec![beat(50)]);
            assert_eq!(loaded.analysis.cues[0].time_ms, 300);
            assert_eq!(loaded.analysis.key.as_deref(), Some("7A"));
            assert_eq!(
                loaded.analysis.three_band_detail,
                detected.three_band_detail
            );
            assert_eq!((loaded.cue_ms, loaded.gain_adjust), (300, 2.5));
        }
        let mut absent = Loaded::default();
        merge_analysis(
            &mut absent,
            detected.clone(),
            &store::Edits::default(),
            &mut Vec::new(),
        );
        assert_eq!(absent.analysis.beats, detected.beats);
        let mut deleted = Loaded::default();
        let edits = store::Edits {
            grid: Some(Vec::new()),
            ..Default::default()
        };
        merge_analysis(&mut deleted, detected, &edits, &mut Vec::new());
        assert!(
            deleted.analysis.beats.is_empty(),
            "an explicit grid deletion stays deleted"
        );
    }
}
