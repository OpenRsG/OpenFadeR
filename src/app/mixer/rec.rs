//! The REC button (Preferences > Advanced > Recordings): starts an `audio::Recorder` on the bus tap (or, with
//! Preferences > Audio > REC source = input, on `audio::input::open(device, pair)`, kept open in [`Rec`]) with the
//! recorder settings. With "start when audio is detected" the button shows ARMED until the first file opens; one
//! recording may make several files (automatic separation), the status line names them, and the writer ending by
//! itself (silence trigger, I/O error) stops the recording on the next tick. Each finished file goes to the
//! library ([`crate::app::library::import_recorded`]: import, show its tag information), once.
//! Next to the files: the cue sheet and tracklist of what led the mix ([`super::sheet`]) and, with the setting on, the
//! control log ([`super::control`]); [`Mixer::follow_rec`] feeds both on every tick of a running recording. The lead is
//! the deck [`lead`] gives (the master while it is on air, else the first deck on air; on air = the history's rule:
//! playing, channel fader times crossfader gain at least -20 dB), told apart by its load count.
//! ponytail: the recorder only reports the file it is writing, so a file split off by silence is handed to the
//! library when the next one opens (or the recording ends), not at the split.

use super::{
    control::{self, Capture},
    sheet::{self, Entry, Tracklist},
    sides, Mixer,
};
use crate::app::{
    browser::history,
    deck::{active, lead, Decks},
    library, prefs, App, DeckState, MixerState,
};
use openfader::audio::{input::Input, rec_path, Recorder};
use openfader::plugin::{event, source};
use serde_json::json;
use slint::{ComponentHandle, Model};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// One recording in progress.
pub(super) struct Rec {
    recorder: Recorder,
    /// The open audio input while REC records it instead of the master (dropped after the recorder: it closes the stream).
    input: Option<Input>,
    /// The file the recorder last reported writing (`None` while armed, waiting for audio).
    seen: Option<PathBuf>,
    /// Every track that led the mix so far, on the recorder's clock.
    tracks: Tracklist,
    /// The files written so far with where each began (ms on the recorder's clock), for the running sheets.
    files: Vec<(PathBuf, u64)>,
    /// The control log while the setting is on.
    controls: Option<Capture>,
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or(p.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// "M:SS" of an elapsed time.
pub(super) fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The status line after a recording ended: the files it saved (an automatic separation makes several), its length
/// in all, and a warning for frames the disk could not take; `auto` = it ended by itself (silence).
fn saved_text(files: &[PathBuf], total: Duration, dropped: u64, auto: bool) -> String {
    let mut text = match files {
        [] => return tr!("REC stopped: no audio came, nothing saved").into(),
        [f] => tr!("Saved {} ({})", file_name(f), clock(total)),
        [first, .., last] => tr!(
            "Saved {} files: {} … {} ({} in all)",
            files.len(),
            file_name(first),
            file_name(last),
            clock(total)
        ),
    };
    if auto {
        text.insert_str(0, tr!("REC ended after silence. "));
    }
    if dropped > 0 {
        text += &tr!(", {} frames lost: the disk could not keep up", dropped);
    }
    text
}

impl Mixer {
    /// REC button: starts a recording of the post-master mix, or stops and finalises the running one (also cancels an armed one).
    pub(super) fn toggle_rec(&self) {
        if self.rec.borrow().is_some() {
            self.end_rec(false);
        } else {
            self.start_rec();
        }
    }

    /// Records the master (the bus tap), or with Preferences > Audio > Input channels set to record the input, that input pair.
    fn start_rec(&self) {
        let p = prefs::get();
        let (tap, input) = if p.audio.rec_input() {
            match openfader::audio::input::open(&p.audio.input_device, p.audio.input_pair) {
                Ok(i) => (i.tap(), Some(i)),
                Err(e) => return self.set_status(tr!("REC: input: {}", e)),
            }
        } else {
            match &self.bus {
                Some(bus) => (bus.tap(), None),
                None => {
                    return self.set_status(tr!("REC: no audio output, nothing to record").into());
                }
            }
        };
        let a = p.advanced;
        let folder = a.rec_folder();
        match Recorder::start(
            tap,
            a.rec_options(),
            Box::new(move || rec_path(folder.as_deref())),
        ) {
            Ok(recorder) => {
                let seen = recorder.path();
                self.set_status(match &seen {
                    Some(p) => tr!("REC → {}", file_name(p)),
                    None => tr!("REC armed: waiting for audio").into(),
                });
                self.edit_bus(|b| {
                    (b.rec, b.rec_armed, b.rec_text) =
                        (true, seen.is_none(), clock(Duration::ZERO).into())
                });
                self.imported.borrow_mut().clear();
                openfader::plugin::event::fire("recording_started", serde_json::json!({}));
                *self.rec.borrow_mut() = Some(Rec {
                    recorder,
                    input,
                    seen,
                    tracks: Tracklist::default(),
                    files: Vec::new(),
                    controls: None,
                });
            }
            Err(e) => self.set_status(tr!("REC failed: {}", e)),
        }
    }

    /// Stops the recording and shows what it saved; `auto` = the writer ended by itself (silence, or an I/O error).
    fn end_rec(&self, auto: bool) {
        let Some(Rec {
            recorder,
            input: _input,
            seen,
            tracks,
            files: _,
            controls,
        }) = self.rec.borrow_mut().take()
        else {
            return;
        };
        self.edit_bus(|b| (b.rec, b.rec_armed) = (false, false));
        let so_far = clock(recorder.elapsed());
        let text = match recorder.stop() {
            Ok(t) => {
                let mut sidecars = Vec::new();
                let (cue, list, min) = prefs::with(|p| {
                    (
                        p.advanced.rec_cue,
                        p.advanced.rec_tracklist,
                        p.advanced.min_track(),
                    )
                });
                let mut warning = String::new();
                if cue || list {
                    match sheet::write(&t, tracks.entries(), min, cue, list) {
                        Ok(made) => sidecars = made,
                        Err(e) => warning = tr!(". Cue sheet not written: {}", e),
                    }
                }
                sidecars.extend(controls.and_then(Capture::finish));
                self.import(&t.files);
                event::fire(
                    "recording_stopped",
                    json!({
                        "files": t.files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(),
                        "sidecars": sidecars.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(),
                    }),
                );
                saved_text(&t.files, t.duration(), t.dropped, auto) + &warning
            }
            Err(e) => tr!(
                "REC stopped at {}, {} may be incomplete: {}",
                so_far,
                seen.as_deref().map_or(tr!("the file").into(), file_name),
                e
            ),
        };
        self.set_status(text);
    }

    /// Follows the running recording: the clock, ARMED → recording, a new file after an automatic split (the
    /// previous one is finished then: imported), and the writer ending by itself.
    pub(super) fn tick_rec(&self) {
        let mut guard = self.rec.borrow_mut();
        let Some(r) = guard.as_mut() else { return };
        if r.recorder.failed() {
            drop(guard);
            return self.end_rec(true);
        }
        let (path, elapsed) = (r.recorder.path(), r.recorder.elapsed());
        let previous = (path != r.seen).then(|| std::mem::replace(&mut r.seen, path.clone()));
        let previous_seen = previous.is_some();
        if let (true, Some(p)) = (previous_seen, &path) {
            r.files.push((p.clone(), elapsed.as_millis() as u64));
        }
        drop(guard);
        if let Some(prev) = previous {
            if let Some(done) = prev {
                self.import(&[done]);
            }
            if let Some(p) = &path {
                self.set_status(tr!("REC → {}", file_name(p)));
            }
        }
        self.edit_bus(|b| (b.rec_armed, b.rec_text) = (path.is_none(), clock(elapsed).into()));
        self.follow_rec(elapsed, path.as_deref(), previous_seen);
    }

    /// Hands finished recordings to the library (import, show the tag information), each once.
    fn import(&self, files: &[PathBuf]) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (import, tag) = prefs::with(|p| (p.advanced.rec_import, p.advanced.rec_tag_window));
        for f in files {
            if !self.imported.borrow().contains(f) {
                self.imported.borrow_mut().push(f.clone());
                library::import_recorded(&app, f, import, tag);
            }
        }
    }

    /// The deck that leads the mix now, with its load count ([`lead`]).
    fn lead(&self, app: &App) -> Option<(usize, u32)> {
        let sides = sides(app);
        let faders = app.global::<MixerState>().get_channels();
        let master = usize::try_from(app.global::<DeckState>().get_master()).ok();
        lead(&self.decks, master, |d| {
            let i = d.index();
            let fader = faders.row_data(i)?.fader;
            if i >= active() || !history::on_air(d.playing(), fader, sides[i]) {
                return None;
            }
            Some((i, d.serial()?))
        })
    }

    /// Once per tick of a running recording: notes the track that took over (cue sheet, tracklist, the `recording_track`
    /// event) and, with the setting on, logs the controls; `new_file` = the recorder opened `file` since the last tick.
    fn follow_rec(&self, elapsed: Duration, file: Option<&Path>, new_file: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let lead = self.lead(&app);
        let state =
            prefs::with(|p| p.advanced.rec_controls).then(|| control::sample(&app, &self.decks));
        let mut guard = self.rec.borrow_mut();
        let Some(r) = guard.as_mut() else { return };
        let at = elapsed.as_millis() as u64;
        let took_over = r
            .tracks
            .observe(at, lead, |at| {
                entry(&app, &self.decks, lead.map_or(0, |l| l.0), at)
            })
            .cloned();
        let changed = took_over.is_some() || new_file;
        if let (Some(e), Some((deck, _))) = (took_over, lead) {
            let track = self.decks[deck].with_track(source::track_json);
            event::fire(
                "recording_track",
                json!({"deck": deck + 1, "position_ms": e.at_ms, "track": track}),
            );
        }
        if changed && !r.files.is_empty() {
            let (cue, list, min) = prefs::with(|p| {
                (
                    p.advanced.rec_cue,
                    p.advanced.rec_tracklist,
                    p.advanced.min_track(),
                )
            });
            if cue || list {
                // best effort: the final write at the end reports a failure
                let _ = sheet::write_live(&r.files, at, r.tracks.entries(), min, cue, list);
            }
        }
        match (state, r.controls.as_mut()) {
            (Some(s), Some(c)) => {
                if let Some(f) = file.filter(|_| new_file) {
                    c.file(f);
                }
                c.tick(s);
            }
            (Some(s), None) => {
                let mut c = Capture::start();
                if let Some(f) = file {
                    c.file(f);
                }
                c.tick(s);
                r.controls = Some(c);
            }
            (None, _) => r.controls = None,
        }
    }
}

/// The tracklist entry for what deck `d` shows now.
fn entry(app: &App, decks: &Decks, d: usize, at_ms: u64) -> Entry {
    let v = app
        .global::<DeckState>()
        .get_decks()
        .row_data(d)
        .unwrap_or_default();
    let title = match v.title.as_str() {
        "" => decks[d]
            .with_track(|t| {
                t.file
                    .as_deref()
                    .and_then(Path::file_stem)
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .flatten()
            .unwrap_or_default(),
        t => t.to_owned(),
    };
    Entry {
        at_ms,
        artist: v.artist.to_string(),
        title,
        bpm: f64::from(v.bpm),
        key: v.key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec_clock_shows_minutes_and_seconds() {
        assert_eq!(clock(Duration::ZERO), "0:00");
        assert_eq!(clock(Duration::from_millis(59_999)), "0:59");
        assert_eq!(clock(Duration::from_secs(61)), "1:01");
        assert_eq!(clock(Duration::from_secs(3725)), "62:05");
    }

    #[test]
    fn saved_text_names_every_file_and_the_end_reason() {
        let f = |n: &str| PathBuf::from(format!("/rec/{n}.wav"));
        let t = Duration::from_secs(125);
        assert_eq!(saved_text(&[f("a")], t, 0, false), "Saved a.wav (2:05)");
        assert_eq!(
            saved_text(&[f("a"), f("b"), f("c")], t, 0, false),
            "Saved 3 files: a.wav … c.wav (2:05 in all)"
        );
        assert_eq!(
            saved_text(&[f("a")], t, 0, true),
            "REC ended after silence. Saved a.wav (2:05)"
        );
        assert!(
            saved_text(&[f("a")], t, 7, false)
                .ends_with("7 frames lost: the disk could not keep up")
        );
        assert!(saved_text(&[], Duration::ZERO, 0, false).contains("nothing saved"));
    }
}
