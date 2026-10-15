//! The REC button (Preferences > Advanced > Recordings): starts an `audio::Recorder` on the bus tap (or, with
//! Preferences > Audio > REC source = input, on `audio::input::open(device, pair)`, kept open in [`Rec`]) with the
//! recorder settings. With "start when audio is detected" the button shows ARMED until the first file opens; one
//! recording may make several files (automatic separation), the status line names them, and the writer ending by
//! itself (silence trigger, I/O error) stops the recording on the next tick. Each finished file goes to the
//! library ([`crate::app::library::import_recorded`]: import, show its tag information), once.
//! ponytail: the recorder only reports the file it is writing, so a file split off by silence is handed to the
//! library when the next one opens (or the recording ends), not at the split.

use super::Mixer;
use crate::app::{library, prefs};
use openfader::audio::{Recorder, input::Input, rec_path};
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
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or(p.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// "M:SS" of an elapsed time.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The status line after a recording ended: the files it saved (an automatic separation makes several), its length
/// in all, and a warning for frames the disk could not take; `auto` = it ended by itself (silence).
fn saved_text(files: &[PathBuf], total: Duration, dropped: u64, auto: bool) -> String {
    let mut text = match files {
        [] => return "REC stopped: no audio came, nothing saved".into(),
        [f] => format!("Saved {} ({})", file_name(f), clock(total)),
        [first, .., last] => format!(
            "Saved {} files: {} … {} ({} in all)",
            files.len(),
            file_name(first),
            file_name(last),
            clock(total)
        ),
    };
    if auto {
        text.insert_str(0, "REC ended after silence. ");
    }
    if dropped > 0 {
        text += &format!(", {dropped} frames lost: the disk could not keep up");
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
                Err(e) => return self.set_status(format!("REC: input: {e}")),
            }
        } else {
            match &self.bus {
                Some(bus) => (bus.tap(), None),
                None => return self.set_status("REC: no audio output, nothing to record".into()),
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
                    Some(p) => format!("REC → {}", file_name(p)),
                    None => "REC armed: waiting for audio".into(),
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
                });
            }
            Err(e) => self.set_status(format!("REC failed: {e}")),
        }
    }

    /// Stops the recording and shows what it saved; `auto` = the writer ended by itself (silence, or an I/O error).
    fn end_rec(&self, auto: bool) {
        let Some(Rec {
            recorder,
            input: _input,
            seen,
        }) = self.rec.borrow_mut().take()
        else {
            return;
        };
        self.edit_bus(|b| (b.rec, b.rec_armed) = (false, false));
        let so_far = clock(recorder.elapsed());
        let text = match recorder.stop() {
            Ok(t) => {
                self.import(&t.files);
                openfader::plugin::event::fire(
                    "recording_stopped",
                    serde_json::json!({"files": t.files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>()}),
                );
                saved_text(&t.files, t.duration(), t.dropped, auto)
            }
            Err(e) => format!(
                "REC stopped at {so_far}, {} may be incomplete: {e}",
                seen.as_deref().map_or("the file".into(), file_name)
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
        drop(guard);
        if let Some(prev) = previous {
            if let Some(done) = prev {
                self.import(&[done]);
            }
            if let Some(p) = &path {
                self.set_status(format!("REC → {}", file_name(p)));
            }
        }
        self.edit_bus(|b| (b.rec_armed, b.rec_text) = (path.is_none(), clock(elapsed).into()));
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
