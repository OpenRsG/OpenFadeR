//! Split screen ↔ ui/split.slint (`LayoutState`, `SplitWindow`): the browser in a window of its own at the bottom right of the screen. Another browser panel is displayed. … Select a track in the split screen,
//! and click a deck at the top of the screen. The track then is loaded to the deck."]. The status bar's split button and
//! View > Split Screen open and close it; the second status bar button brings it forward.
//!
//! The window holds the whole `Browser` component (tree, Tag List, My Playlists, Tags, filter, preview, track info, right-click menu,
//! Playlist Palette, drag and drop) and a strip with a LOAD chip and a drop target per deck. Like the VIDEO output window
//! ([`super::video`]) it is a second Slint window component, and its globals are its own: [`mirror`] copies the browser's state
//! from the main window into them and routes the window's callbacks to the main window's handlers. So the browser code is the one in
//! [`super::browser`]: there is one browser state, shown twice. Choosing a playlist, a search or a sort in one window is therefore
//! seen in the other too.
//! Dialogs a menu opens (the library menu's file dialogs, Preferences) appear in the main window, and the keyboard shortcuts
//! of the main window are not repeated here but Ctrl+F (search) and T (Tag List).

mod mirror;

use super::{App, DeckState, LayoutState, SplitWindow, prefs};
use mirror::Mirror;
use slint::{
    CloseRequestResponse, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

pub struct Split {
    app: slint::Weak<App>,
    window: RefCell<Option<(SplitWindow, Mirror)>>,
    decks: Rc<VecModel<SharedString>>,
    _timer: Timer,
}

/// Opens nothing yet: the window is made when the split button is first pressed. Keep the guard for as long as the app runs.
pub fn wire(app: &App) -> Rc<Split> {
    let split = Rc::new_cyclic(|me: &std::rc::Weak<Split>| {
        let weak = me.clone();
        let timer = Timer::default();
        // What the decks hold changes the strip's labels.
        timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
            if let Some(s) = weak.upgrade() {
                s.show_decks();
            }
        });
        Split {
            app: app.as_weak(),
            window: RefCell::default(),
            decks: Rc::default(),
            _timer: timer,
        }
    });
    let state = app.global::<LayoutState>();
    let w = Rc::downgrade(&split);
    state.on_split_toggle({
        let w = w.clone();
        move || {
            if let Some(s) = w.upgrade() {
                s.toggle();
            }
        }
    });
    state.on_split_switch({
        let w = w.clone();
        move || {
            if let Some(win) = w.upgrade().and_then(|s| s.window()) {
                let _ = win.show();
            }
        }
    });
    // The app ends with its main window, the split window going with it.
    app.window().on_close_requested(move || {
        if let Some(s) = w.upgrade() {
            s.hide();
        }
        CloseRequestResponse::HideWindow
    });
    split
}

impl Split {
    fn window(&self) -> Option<SplitWindow> {
        self.window.borrow().as_ref().map(|(w, _)| w.clone_strong())
    }

    fn toggle(self: &Rc<Self>) {
        match self.window().filter(|w| w.window().is_visible()) {
            Some(_) => self.hide(),
            None => self.open(),
        }
    }

    fn hide(&self) {
        if let Some((w, m)) = &*self.window.borrow() {
            m.pause();
            let _ = w.hide();
        }
        self.set_open(false);
    }

    fn set_open(&self, open: bool) {
        if let Some(app) = self.app.upgrade() {
            app.global::<LayoutState>().set_split_open(open);
        }
    }

    fn open(self: &Rc<Self>) {
        if self.window.borrow().is_none() {
            let Some(pair) = self.create() else { return };
            *self.window.borrow_mut() = Some(pair);
        }
        if let Some((w, m)) = &*self.window.borrow() {
            m.run();
            let _ = w.show();
        }
        self.show_decks();
        self.set_open(true);
    }

    /// The window, its look and its callbacks (made once, reused by later opens).
    fn create(self: &Rc<Self>) -> Option<(SplitWindow, Mirror)> {
        let app = self.app.upgrade()?;
        let w = SplitWindow::new().expect("split screen window");
        let mirror = Mirror::new(&app, &w);
        w.set_decks(ModelRc::from(self.decks.clone()));
        prefs::with(|p| prefs::apply_theme(&w, p));
        let weak = w.as_weak();
        prefs::on_change(move |p| {
            if let Some(w) = weak.upgrade() {
                prefs::apply_theme(&w, p);
            }
        });
        let me = Rc::downgrade(self);
        w.window().on_close_requested({
            let me = me.clone();
            move || {
                if let Some(s) = me.upgrade() {
                    s.hide();
                }
                CloseRequestResponse::HideWindow
            }
        });
        w.on_load(move |deck| {
            if let Some(app) = me.upgrade().and_then(|s| s.app.upgrade()) {
                app.global::<DeckState>().invoke_load(deck);
            }
        });
        Some((w, mirror))
    }

    /// One strip entry per deck in use: the loaded track's title, empty when there is none.
    fn show_decks(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let state = app.global::<DeckState>();
        let titles: Vec<SharedString> = state
            .get_decks()
            .iter()
            .take(state.get_count() as usize)
            .map(|d| {
                if d.loaded {
                    d.title
                } else {
                    SharedString::new()
                }
            })
            .collect();
        if self.decks.iter().collect::<Vec<_>>() != titles {
            self.decks.set_vec(titles);
        }
    }
}
