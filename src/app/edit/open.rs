//! OPEN SELECTED: the browser's selected track is decoded (and analysed where the library has no grid or
//! waveform) on a worker thread; [`State::poll_loading`] picks the result up from the tick. The library's
//! analysis (grid and waveform) comes from `lib.analysis`, like a deck LOAD.

use super::{Open, State};
use openfader::{analysis::Analysis, edit::load};
use std::sync::mpsc::{TryRecvError, channel};

impl State {
    /// The mode was shown: with nothing open yet, load the selected track.
    pub(super) fn opened(&self) {
        self.dirty.set(true);
        if self.open.borrow().is_none()
            && self.loading.borrow().is_none()
            && self.selected.borrow().is_some()
        {
            self.open_selected();
        }
    }

    pub(super) fn open_selected(&self) {
        let Some(t) = self.selected.borrow().clone() else {
            return self.say("select a track in the library first");
        };
        let Some(file) = t.file.clone() else {
            return self.say("the selected track has no audio file");
        };
        if self
            .open
            .borrow()
            .as_ref()
            .is_some_and(|o| o.edit.is_edited())
            && !self.discard.replace(true)
        {
            return self.say("the edits are not rendered and will be lost: press OPEN SELECTED again to discard them");
        }
        self.stop();
        let a = self.lib.analysis(&t).unwrap_or_default();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(
                load::load(
                    &file,
                    Some(&a).filter(|a: &&Analysis| {
                        !a.beats.is_empty() || !a.three_band_detail.is_empty()
                    }),
                )
                .map(|l| (t, l)),
            );
        });
        *self.loading.borrow_mut() = Some(rx);
        self.say("loading and analysing…");
    }

    pub(super) fn poll_loading(&self) {
        let got = match self.loading.borrow().as_ref().map(|rx| rx.try_recv()) {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("the load stopped unexpectedly".to_string())
            }
            Some(Ok(r)) => r,
        };
        *self.loading.borrow_mut() = None;
        match got {
            Ok((t, l)) => {
                let o = Open::new(t, l);
                self.say(format!(
                    "opened {} — {} beats",
                    o.track.title,
                    o.lines.len()
                ));
                *self.open.borrow_mut() = Some(o);
                *self.sel.borrow_mut() = None;
                *self.rendered.borrow_mut() = None;
                self.cursor.set(0);
                self.start_ms.set(0.0);
                self.ppm.set(None);
                self.discard.set(false);
                self.invalidate();
            }
            Err(e) => self.say(format!("cannot open: {e}")),
        }
    }
}
