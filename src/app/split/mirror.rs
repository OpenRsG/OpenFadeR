//! A second window's copy of the browser's globals, kept in step with the main window's. Every window root has globals of its own, so
//! the split-screen window cannot share the browser's state, but it can share what is behind it: `ui/build.rs` reads the declarations of the
//! globals in `ui/*.slint` and writes [`generated::sync`] (the main window's properties copied over, shared models and all; a property the UI
//! sets in either window follows the window that changed it last) and [`generated::forward`] (each callback of the split window's globals
//! calls the main window's, which `src/app/browser/` and the other sections already handle). A property or callback added to one of
//! those globals is mirrored without an edit here.
//!
//! Callbacks run the sync first, so the main window's handler reads what the split window's UI just set (the chosen row, the
//! filter), and again after, so the window sees what the handler changed. Rust changes to the main window's properties arrive on the
//! timer ([`PERIOD`]).

use crate::app::{App, SplitWindow};
use openfader_ui::mirror as generated;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};

/// How often the main window's changes are copied over while the split window is open.
const PERIOD: Duration = Duration::from_millis(50);

pub struct Mirror {
    sync: Rc<dyn Fn()>,
    timer: Timer,
}

impl Mirror {
    /// Starts equal to the main window, forwards the split window's callbacks; call [`Mirror::run`] while it is open.
    pub fn new(app: &App, win: &SplitWindow) -> Self {
        let step = RefCell::new(generated::sync(app.as_weak(), win.as_weak()));
        // A callback may be invoked while a sync runs (a handler sets a property): that one is skipped, the timer catches up.
        let sync: Rc<dyn Fn()> = Rc::new(move || {
            if let Ok(mut f) = step.try_borrow_mut() {
                f();
            }
        });
        generated::forward(&app.as_weak(), win, &sync);
        Mirror {
            sync,
            timer: Timer::default(),
        }
    }

    /// Copies now and every [`PERIOD`] until [`Mirror::pause`].
    pub fn run(&self) {
        (self.sync)();
        let sync = self.sync.clone();
        self.timer
            .start(TimerMode::Repeated, PERIOD, move || sync());
    }

    pub fn pause(&self) {
        self.timer.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BrowserState;
    use slint::{DataTransfer, Model, ModelRc, StandardListViewItem, VecModel};
    use std::cell::Cell;

    #[test]
    fn the_split_window_follows_the_main_one_and_calls_its_handlers() {
        crate::app::headless::init();
        let (app, win) = (App::new().unwrap(), SplitWindow::new().unwrap());
        let main = app.global::<BrowserState>();
        let rows: Rc<VecModel<ModelRc<StandardListViewItem>>> = Rc::default();
        main.set_tracks(ModelRc::from(rows.clone()));
        main.set_status("0 of 0 tracks".into());
        let heard = Rc::new(Cell::new(-1));
        main.on_track_selected({
            let (heard, app) = (heard.clone(), app.as_weak());
            move |row| {
                heard.set(row);
                // the handler reads the in-out value the split window's UI set, and answers through a property
                let app = app.upgrade().unwrap();
                let s = app.global::<BrowserState>();
                s.set_status(format!("row {row} of {}", s.get_current_row()).into());
            }
        });
        main.on_drag_track(|row| {
            DataTransfer::from(slint::SharedString::from(format!("track {row}")))
        });
        let mirror = Mirror::new(&app, &win);
        mirror.run();
        let split = win.global::<BrowserState>();

        // properties and models are copied, a shared model is the same one
        assert_eq!(split.get_status(), "0 of 0 tracks");
        rows.push(ModelRc::new(VecModel::from(vec![
            StandardListViewItem::from("a"),
        ])));
        assert_eq!(split.get_tracks().row_count(), 1);

        // the split window's UI chose a row: the main window's handler hears it, with the row set before the call, and its answer
        // comes back at once
        split.set_current_row(4);
        split.invoke_track_selected(4);
        assert_eq!(heard.get(), 4);
        assert_eq!(main.get_current_row(), 4);
        assert_eq!(split.get_status(), "row 4 of 4");

        // an in-out property the main window changes (Rust, a shortcut) reaches the split window
        main.set_min_rating(3);
        mirror.run();
        assert_eq!(split.get_min_rating(), 3);
        // and the other way, with no callback to carry it, on the next step
        split.set_min_rating(5);
        mirror.run();
        assert_eq!(main.get_min_rating(), 5);

        // a pure callback returns the handler's value
        assert_eq!(split.invoke_drag_track(7).plain_text().unwrap(), "track 7");
    }
}
