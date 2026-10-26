//! The PREFERENCES dialog: page table, row refresh, clicks and the start-up wiring.

use super::{
    advanced, analysis, audio, controller, dj_system, extensions, keyboard, libraries, live, look,
    panes, view, Prefs, Row,
};
use crate::app::{App, PrefRowData, PrefsState};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, time::Duration};

thread_local! {
    static APP: RefCell<Option<slint::Weak<App>>> = const { RefCell::new(None) };
}

/// The window [`wire`] connected, while it is alive.
pub(super) fn app() -> Option<App> {
    APP.with_borrow(|a| a.as_ref().and_then(slint::Weak::upgrade))
}

/// The dialog pages in category order: rows and click handler of each (see the module doc).
type Page = (fn(&Prefs) -> Vec<Row>, fn(&mut Prefs, &str, usize) -> bool);
pub(super) const PAGES: [Page; 10] = [
    (view::rows, view::choose),
    (audio::rows, audio::choose),
    (analysis::rows, analysis::choose),
    (controller::rows, controller::choose),
    (keyboard::rows, keyboard::choose),
    (extensions::rows, extensions::choose),
    (dj_system::rows, dj_system::choose),
    (advanced::rows, advanced::choose),
    (|_| Vec::new(), |_, _, _| false), // THEMES (8) has its own page, no rows
    (libraries::rows, libraries::choose),
];

/// Redraws the open category's rows (call after changing a setting kept outside [`Prefs`], e.g. in store.db).
pub fn refresh() {
    let Some(app) = app() else { return };
    let s = app.global::<PrefsState>();
    if !s.get_visible() {
        return; // rebuilt when the dialog opens (`category-chosen`); page rows may be costly (device lists)
    }
    let rows = PAGES
        .get(s.get_category().max(0) as usize)
        .map_or_else(Vec::new, |(rows, _)| rows(&live::get()));
    s.set_rows(ModelRc::new(VecModel::from(
        rows.into_iter().map(PrefRowData::from).collect::<Vec<_>>(),
    )));
}

fn choose(key: &str, i: usize) {
    if !live::run_action(key) {
        live::update(|p| {
            if !p.choose(key, i) {
                PAGES.iter().any(|(_, c)| c(p, key, i));
            }
        });
    }
    refresh();
}

/// Remembers the window size: the size lives in Slint, so it is polled.
fn watch_window(app: &App) -> Timer {
    let weak = app.as_weak();
    let win = app.window();
    let last = RefCell::new(win.size().to_logical(win.scale_factor()));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(app) = weak.upgrade() else { return };
        panes::save(&app);
        let win = app.window();
        let now = win.size().to_logical(win.scale_factor());
        if std::mem::replace(&mut *last.borrow_mut(), now) != now
            && now.width > 0.0
            && now.height > 0.0
        {
            live::update(|p| {
                (p.window_w, p.window_h) = (now.width.round() as u32, now.height.round() as u32)
            });
        }
    });
    timer
}

/// Loads the settings, sizes the window, connects the PREFERENCES dialog and starts the layout watcher; keep
/// the returned timer alive for as long as the window runs. Call before the sections that read [`live::get`].
pub fn wire(app: &App) -> Timer {
    let p = super::load();
    live::set(p.clone());
    APP.with_borrow_mut(|a| *a = Some(app.as_weak()));
    if p.window_w > 0 && p.window_h > 0 {
        app.window().set_size(slint::LogicalSize::new(
            p.window_w as f32,
            p.window_h as f32,
        ));
    }

    let s = app.global::<PrefsState>();
    s.set_path(
        super::path()
            .map_or_else(String::new, |p| p.display().to_string())
            .into(),
    );
    s.on_chose(|key, i| choose(&key, i.max(0) as usize));
    s.on_text_edited(|key, v| {
        live::run_text(&key, &v);
        dj_system::text(&key, &v); // its row keys are made from the plugins' schemas, so it routes them itself
        refresh();
    });
    s.on_category_chosen(|_| refresh());
    dj_system::init(app);
    live::on_change(|_| refresh());
    controller::wire();
    extensions::wire();
    advanced::wire();
    #[cfg(feature = "stems")]
    super::lowpower::wire();
    keyboard::wire(app);
    refresh();
    look::apply(app, &p);
    panes::apply(app, &p.view);
    let weak = app.as_weak();
    live::on_change(move |p| {
        if let Some(app) = weak.upgrade() {
            look::apply(&app, p);
        }
    });
    watch_window(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_can_read_and_edit_current_preferences() {
        std::thread::spawn(|| {
            live::set(Prefs {
                tempo_range_default: 2,
                ..Default::default()
            });
            live::on_action("analyze_now", || {
                let range = live::with(|p| p.tempo_range_default);
                live::set_for_test(|p| p.tempo_range_default = range + 1);
            });
            choose("analyze_now", 0);
            assert_eq!(live::with(|p| p.tempo_range_default), 3);
        })
        .join()
        .unwrap();
    }
}
