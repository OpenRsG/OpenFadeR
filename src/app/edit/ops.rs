//! The edit buttons and what leaves the editor: copy / cut / paste / insert / delete / clear (mute) /
//! insert silence / duplicate, undo / redo, preview, RENDER (a new WAV, never over the source) and TO DECK
//! (render, then a deck LOAD of the new file). An edit stops the preview, then selects what it made.

use super::{Open, State};
use openfader::{
    edit::{play::Play, wav},
    library::Track,
    paths,
};
use std::{ops::Range, path::PathBuf, sync::Arc};

impl State {
    pub(super) fn stop(&self) {
        if self.play.borrow_mut().take().is_some() {
            self.dirty.set(true);
        }
    }

    /// Runs one edit `f` on the selection (or the empty range at the playhead), then selects what it returns.
    fn apply(
        &self,
        what: &str,
        need_sel: bool,
        f: impl FnOnce(&mut Open, Range<usize>) -> Option<Range<usize>>,
    ) {
        self.discard.set(false);
        let sel = self.sel();
        let cursor = self.cursor.get();
        let mut open = self.open.borrow_mut();
        let Some(o) = open.as_mut() else {
            return self.say("open a track first: OPEN SELECTED");
        };
        if need_sel && sel.is_none() {
            return self.say(format!(
                "{what}: select a range first (drag on the waveform)"
            ));
        }
        self.stop();
        match f(o, sel.unwrap_or(cursor..cursor)) {
            Some(r) => {
                o.refresh();
                self.cursor.set(r.start);
                self.say(format!(
                    "{what}: done ({:.2} s)",
                    r.len() as f64 / o.edit.rate() as f64
                ));
                *self.sel.borrow_mut() = (!r.is_empty()).then_some(r);
                self.invalidate();
            }
            None => self.say(format!("{what}: nothing to do")),
        }
    }

    fn need_clip(&self) -> bool {
        let empty = self
            .open
            .borrow()
            .as_ref()
            .is_some_and(|o| o.edit.clip_len() == 0);
        if empty {
            self.say("the clipboard is empty: copy or cut a range first");
        }
        !empty
    }

    pub(super) fn copy(&self) {
        let (sel, mut open) = (self.sel(), self.open.borrow_mut());
        let Some(o) = open.as_mut() else {
            return self.say("open a track first: OPEN SELECTED");
        };
        match sel.filter(|r| o.edit.copy(r.clone())) {
            Some(r) => self.say(format!(
                "copied {:.2} s",
                r.len() as f64 / o.edit.rate() as f64
            )),
            None => self.say("copy: select a range first (drag on the waveform)"),
        }
    }

    pub(super) fn cut(&self) {
        self.apply("cut", true, |o, r| o.edit.cut(r));
    }

    /// Overwrites from the selection start / playhead with the clipboard.
    pub(super) fn paste(&self) {
        if self.need_clip() {
            self.apply("paste", false, |o, r| o.edit.paste(r.start));
        }
    }

    /// Inserts the clipboard at the selection start / playhead, the rest moves down.
    pub(super) fn insert(&self) {
        if self.need_clip() {
            self.apply("insert", false, |o, r| o.edit.insert(r.start));
        }
    }

    pub(super) fn delete(&self) {
        self.apply("delete", true, |o, r| o.edit.delete(r));
    }

    /// Mutes the selection (its length stays).
    pub(super) fn clear(&self) {
        self.apply("mute", true, |o, r| o.edit.clear(r));
    }

    /// Silence the size of the selection, or four beats, at the playhead.
    pub(super) fn silence(&self) {
        self.apply("insert silence", false, |o, r| {
            let n = if r.is_empty() {
                o.bar_frames()
            } else {
                r.len()
            };
            o.edit.insert_silence(r.start, n)
        });
    }

    pub(super) fn duplicate(&self) {
        self.apply("duplicate", true, |o, r| o.edit.duplicate(r));
    }

    pub(super) fn select_all(&self) {
        if let Some(o) = self.open.borrow().as_ref() {
            *self.sel.borrow_mut() = Some(0..o.edit.len());
            self.invalidate();
        }
    }

    pub(super) fn history(&self, undo: bool) {
        self.stop();
        let mut open = self.open.borrow_mut();
        let Some(o) = open.as_mut() else { return };
        let done = if undo { o.edit.undo() } else { o.edit.redo() };
        let name = if undo { "undo" } else { "redo" };
        if done {
            o.refresh();
            self.cursor.set(self.cursor.get().min(o.edit.len()));
            *self.sel.borrow_mut() = None;
            self.invalidate();
        }
        self.say(if done {
            name.to_string()
        } else {
            format!("nothing to {name}")
        });
    }

    /// Starts the preview of the arrangement at frame `from`.
    pub(super) fn play_from(&self, from: usize) {
        let pcm = match self.open.borrow().as_ref() {
            Some(o) => Arc::new(o.edit.render()),
            None => return,
        };
        self.stop();
        let from = if from >= pcm.samples.len() / 2 {
            0
        } else {
            from
        };
        match Play::start(pcm, from) {
            Ok(p) => *self.play.borrow_mut() = Some(p),
            Err(e) => self.say(format!("cannot play: {e}")),
        }
        self.dirty.set(true);
    }

    pub(super) fn play_toggle(&self) {
        if self.play.borrow().is_some() {
            self.stop();
        } else if self.open.borrow().is_some() {
            self.play_from(self.cursor.get());
        }
    }

    pub(super) fn rewind(&self) {
        let playing = self.play.borrow().is_some();
        self.cursor.set(0);
        self.start_ms.set(0.0);
        if playing {
            self.play_from(0);
        }
        self.invalidate();
    }

    /// The arrangement as a WAV under the output folder; the file of the last render when nothing changed since.
    fn render_file(&self) -> Result<PathBuf, String> {
        let open = self.open.borrow();
        let o = open.as_ref().ok_or("open a track first: OPEN SELECTED")?;
        if let Some((edl, path)) = &*self.rendered.borrow() {
            if edl == o.edit.edl() {
                return Ok(path.clone());
            }
        }
        let path = wav::new_path(&o.track.title).map_err(|e| e.to_string())?;
        paths::at(wav::write(&path, &o.edit), &path)?;
        *self.rendered.borrow_mut() = Some((o.edit.edl().clone(), path.clone()));
        Ok(path)
    }

    pub(super) fn render(&self) {
        self.discard.set(false);
        match self.render_file() {
            Ok(p) => self.say(format!("rendered {}", p.display())),
            Err(e) => self.say(format!("render failed: {e}")),
        }
    }

    /// Renders (unless already) and LOADs the new file on deck `i`.
    pub(super) fn to_deck(&self, i: usize) {
        let path = match self.render_file() {
            Ok(p) => p,
            Err(e) => return self.say(format!("render failed: {e}")),
        };
        let Some(deck) = self.decks.get(i) else {
            return;
        };
        let (src, secs) = match self.open.borrow().as_ref() {
            Some(o) => (o.track.clone(), o.edit.len() as i64 / o.edit.rate() as i64),
            None => return,
        };
        let t = Track {
            title: format!("{} (edit)", src.title),
            artist: src.artist,
            key: src.key,
            bpm_x100: src.bpm_x100,
            length_s: secs,
            file: Some(path.clone()),
            ..Track::default()
        };
        deck.load(&t, &*self.lib);
        self.say(format!("loaded on deck {}: {}", i + 1, path.display()));
    }
}
