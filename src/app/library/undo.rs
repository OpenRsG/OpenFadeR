//! Edit > Undo / Redo (Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y): steps the store's change journal back and forth, then the browser and the
//! loaded decks read what changed. The menu labels (`LibraryState.undo-label` / `redo-label`) follow the journal.

use super::{Ctx, LibraryState};
use openfader::store::Store;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::Cell, rc::Rc, time::Duration};

thread_local! {
    static POLL: Timer = Timer::default();
}

/// Undoes (or redoes) the latest step and says which.
pub(super) fn run(c: &Rc<Ctx>, redo: bool) {
    match if redo { c.store.redo() } else { c.store.undo() } {
        Ok(Some(label)) => {
            for d in c.decks.iter() {
                d.reread_edits(&*c.lib);
            }
            crate::app::artwork::changed(c); // reload, jackets and track info
            labels(c);
            c.say(&if redo {
                tr!("redid {}", label)
            } else {
                tr!("undid {}", label)
            });
        }
        Ok(None) => c.say(if redo {
            tr!("nothing to redo")
        } else {
            tr!("nothing to undo")
        }),
        Err(e) => c.say(&if redo {
            tr!("redo: {}", e)
        } else {
            tr!("undo: {}", e)
        }),
    }
}

/// Puts the labels of the next undo / redo step on the menu ("" = none).
fn labels(c: &Ctx) {
    let app = c.app();
    let s = app.global::<LibraryState>();
    let label =
        |l: openfader::store::R<Option<String>>| l.ok().flatten().unwrap_or_default().into();
    s.set_undo_label(label(c.store.undo_label()));
    s.set_redo_label(label(c.store.redo_label()));
}

/// Keeps the menu labels current: the journal's commit count is polled twice a second, the labels are read when it moved.
pub(super) fn wire(c: &Rc<Ctx>) {
    labels(c);
    let (k, last) = (c.clone(), Cell::new(Store::journal_tick()));
    POLL.with(|t| {
        t.start(TimerMode::Repeated, Duration::from_millis(500), move || {
            let tick = Store::journal_tick();
            if last.replace(tick) != tick {
                labels(&k);
            }
        })
    });
}
