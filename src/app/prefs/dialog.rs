//! The PREFERENCES dialog: page table, row refresh, clicks and the start-up wiring.

use super::{
    advanced, analysis, audio, controller, dj_system, extensions, keyboard, libraries, live, look,
    panes, view, Prefs, Row,
};
use crate::app::{App, PluginState, PrefsState};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, time::Duration};

thread_local! {
    static APP: RefCell<Option<slint::Weak<App>>> = const { RefCell::new(None) };
    static ROWS: RefCell<super::filter::Index> = RefCell::new(super::filter::Index::default());
}

/// The window [`wire`] connected, while it is alive.
pub(super) fn app() -> Option<App> {
    APP.with_borrow(|a| a.as_ref().and_then(slint::Weak::upgrade))
}

/// The dialog pages in category order: rows and click handler of each (see the module doc).
type Page = (fn(&Prefs) -> Vec<Row>, fn(&mut Prefs, &str, usize) -> bool);
pub(super) const PAGES: [Page; 12] = [
    (view::rows, view::choose),
    (audio::rows, audio::choose),
    (analysis::rows, analysis::choose),
    (controller::rows, controller::choose),
    (keyboard::rows, keyboard::choose),
    (|_| Vec::new(), |_, _, _| false), // optional extensions have their own manager
    (dj_system::rows, dj_system::choose),
    (advanced::rows, advanced::choose),
    (|_| Vec::new(), |_, _, _| false), // THEMES (8) has its own page, no rows
    (libraries::rows, libraries::choose),
    (|_| Vec::new(), |_, _, _| false), // native compatibility manager
    (extensions::rows, extensions::choose),
];

fn navigation(app: &App) {
    let s = app.global::<PrefsState>();
    let categories = s.get_categories();
    let category = s.get_category();
    if let Some(i) = categories.iter().position(|c| c.id == category) {
        s.set_category_index(i as i32);
    }
    let old = s.get_category_titles();
    if old.row_count() != categories.row_count()
        || categories
            .iter()
            .zip(old.iter())
            .any(|(c, title)| c.title != title)
    {
        s.set_category_titles(ModelRc::new(VecModel::from(
            categories.iter().map(|c| c.title).collect::<Vec<_>>(),
        )));
        s.set_category_icons(ModelRc::new(VecModel::from(
            categories.iter().map(|c| c.icon).collect::<Vec<_>>(),
        )));
    }
}

fn filter(app: &App, query: &str) {
    let s = app.global::<PrefsState>();
    s.set_query(query.into());
    let rows = ROWS.with_borrow(|index| index.rows(query, s.get_section().max(0) as usize));
    s.set_rows(ModelRc::new(VecModel::from(rows)));
    app.global::<PluginState>().invoke_filter(query.into());
}

/// Rebuild only when settings change; typing searches the current page's cached metadata.
pub fn refresh() {
    let Some(app) = app() else { return };
    navigation(&app);
    let s = app.global::<PrefsState>();
    if !s.get_visible() {
        return;
    }
    let rows = PAGES
        .get(s.get_category().max(0) as usize)
        .map_or_else(Vec::new, |(rows, _)| rows(&live::get()));
    let mut index = super::filter::Index::new(rows);
    let old_sections = s.get_sections();
    let selected = if s.get_section() > 0 {
        old_sections.row_data(s.get_section() as usize)
    } else {
        None
    };
    let section = selected
        .and_then(|name| {
            let mut matching = index
                .sections
                .iter()
                .enumerate()
                .filter(|(_, text)| **text == name);
            let first = matching.next().map(|(i, _)| i);
            if matching.next().is_none() {
                first
            } else {
                None
            }
        })
        .unwrap_or(0);
    s.set_section(section as i32);
    s.set_sections(ModelRc::new(VecModel::from(std::mem::take(
        &mut index.sections,
    ))));
    ROWS.with(|cached| *cached.borrow_mut() = index);
    filter(&app, &s.get_query());
}

fn enter(id: i32) {
    let Some(app) = app() else { return };
    let s = app.global::<PrefsState>();
    s.set_category(if id >= 0 && (id as usize) < PAGES.len() {
        id
    } else {
        0
    });
    s.set_query("".into());
    s.set_section(0);
    s.set_help_key("".into());
    refresh();
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
    s.on_category_chosen(enter);
    s.on_filter_changed(|query| {
        if let Some(app) = self::app() {
            filter(&app, &query);
        }
    });
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
