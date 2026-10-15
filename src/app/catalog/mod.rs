//! Repositories in the window ↔ ui/catalog.slint (global `CatalogState`): GET PLUGINS (PREFS > EXTENSIONS) and GET THEMES
//! (PREFS > THEMES) share one list of GitHub repositories ([`openfader::catalog`]) and one read of them.
//!
//! The first time either page opens, and on REFRESH or after an ADD, a worker thread fetches the repositories one by one (the cached
//! zip when there is one, [`openfader::catalog::read`]) and the answer is shown on the UI thread: each repository with what it holds
//! or why it could not be read, a card per plugin ([`plugin`]), per theme ([`theme`]) and per mapping file ([`mapping`], which also offers
//! a mapping for a controller that is connected without one). Installing runs on a worker thread too
//! (plugins: one at a time, [`openfader::catalog::plugins::install`]; mapping files: [`get::install`]); a theme is written by the skin
//! store on the UI thread ([`skin::get`]) because its folders are that thread's. While a job runs the page is `busy`: the buttons wait
//! and `status` says what is going on; the answer is `message` (plugins also as a toast, with the manager's restart banner when a
//! plugin needs one).

mod mapping;
mod plugin;
mod theme;

use super::{midi::get, prefs, skin::Skin, App, CatalogState, RepoView};
use openfader::catalog::{self, list, plugins::Entry, Repo};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
};

pub use mapping::{mapped, unmapped};

/// A repository that was read: its zip, and what it holds.
struct Source {
    zip: PathBuf,
    plugins: Vec<Entry>,
    themes: Vec<Skin>,
    mappings: Vec<get::Entry>,
    /// Mapping files the parser refused.
    bad_mappings: usize,
}

#[derive(Default)]
struct State {
    /// A read has finished (the pages read only the first time they open).
    loaded: bool,
    /// The repositories in list order with what reading each gave.
    repos: Vec<(Repo, Result<Source, String>)>,
    /// The mapping files the app reads now, by file name ([`get::installed`]).
    installed: HashMap<String, String>,
    /// The search box of GET MAPPINGS.
    query: String,
    /// The mapping offered for a connected controller that has none ([`mapping`]).
    offer: Option<get::Entry>,
}

thread_local! {
    static APP: RefCell<Option<slint::Weak<App>>> = const { RefCell::new(None) };
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// A read was asked for while another ran: it starts when that one ends.
    static AGAIN: Cell<bool> = const { Cell::new(false) };
}

fn app() -> Option<App> {
    APP.with_borrow(|a| a.as_ref().and_then(slint::Weak::upgrade))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A job starts: the buttons wait, `what` is shown.
fn start(s: &CatalogState, what: &str) {
    s.set_busy(true);
    s.set_status(what.into());
}

/// The answer of a job, shown under the repository list.
fn say(s: &CatalogState, text: &str, failed: bool) {
    s.set_message(text.into());
    s.set_failed(failed);
}

/// The job ends with `text`; the cards are shown again.
fn report(app: &App, text: &str, failed: bool) {
    let s = app.global::<CatalogState>();
    s.set_busy(false);
    s.set_status("".into());
    say(&s, text, failed);
    show(app);
    if AGAIN.take() {
        read_all(false);
    }
}

/// Fills `CatalogState` from what was read, and the installed plugins and themes as they are now.
fn show(app: &App) {
    let s = app.global::<CatalogState>();
    STATE.with_borrow(|st| {
        let repos: Vec<RepoView> = st
            .repos
            .iter()
            .map(|(repo, read)| {
                let name = repo.to_string();
                match read {
                    Ok(src) => {
                        let mut note = [
                            (src.plugins.len(), "plugin", "plugins"),
                            (src.themes.len(), "theme", "themes"),
                            (src.mappings.len(), "mapping", "mappings"),
                        ]
                        .iter()
                        .filter(|p| p.0 > 0)
                        .map(|&(n, one, many)| plural(n, one, many))
                        .collect::<Vec<_>>()
                        .join(" · ");
                        if src.bad_mappings > 0 {
                            note += &format!(" ({} unreadable)", src.bad_mappings);
                        }
                        RepoView {
                            name: name.into(),
                            note: note.into(),
                            failed: false,
                        }
                    }
                    Err(e) => RepoView {
                        note: e.strip_prefix(&format!("{name}: ")).unwrap_or(e).into(),
                        name: name.into(),
                        failed: true,
                    },
                }
            })
            .collect();
        s.set_repos(ModelRc::new(VecModel::from(repos)));
        let plugins = plugin::views(st);
        s.set_plugins_todo(plugins.iter().filter(|p| p.action != 2).count() as i32);
        s.set_plugins(ModelRc::new(VecModel::from(plugins)));
        s.set_themes(ModelRc::new(VecModel::from(theme::views(st))));
        mapping::show(&s, st);
    });
}

/// Reads every repository of the list on a worker thread (`refresh`: downloading each again) and shows the answer.
fn read_all(refresh: bool) {
    let Some(app) = app() else { return };
    let s = app.global::<CatalogState>();
    if s.get_busy() {
        AGAIN.set(true);
        return;
    }
    start(&s, "Reading the repositories…");
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let repos = list::load();
        let reads: Vec<_> = repos
            .iter()
            .map(|r| {
                let name = r.to_string();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.global::<CatalogState>()
                        .set_status(format!("Reading {name}…").into());
                });
                catalog::read(r, refresh)
            })
            .collect();
        let installed = get::installed();
        let _ = weak.upgrade_in_event_loop(move |app| loaded(&app, repos, reads, installed));
    });
}

/// A read ended: what it found is the state now.
fn loaded(
    app: &App,
    repos: Vec<Repo>,
    reads: Vec<Result<catalog::Found, String>>,
    installed: HashMap<String, String>,
) {
    let repos: Vec<_> = repos
        .into_iter()
        .zip(reads)
        .map(|(repo, read)| {
            let read = read.map(|f| {
                let maps: Vec<_> = f.mappings.iter().map(get::entry).collect();
                Source {
                    zip: f.zip,
                    plugins: f.plugins,
                    themes: f.themes.iter().filter_map(skin_of).collect(),
                    bad_mappings: maps.iter().filter(|m| m.is_err()).count(),
                    mappings: maps.into_iter().flatten().collect(),
                }
            });
            (repo, read)
        })
        .collect();
    let (ok, bad) = (
        repos.iter().filter(|r| r.1.is_ok()).count(),
        repos.iter().filter(|r| r.1.is_err()).count(),
    );
    let (plugins, themes, maps) =
        repos
            .iter()
            .filter_map(|r| r.1.as_ref().ok())
            .fold((0, 0, 0), |n, s| {
                (
                    n.0 + s.plugins.len(),
                    n.1 + s.themes.len(),
                    n.2 + s.mappings.len(),
                )
            });
    let mut text = match repos.len() {
        0 => "No repository: add one below.".to_owned(),
        _ => format!(
            "{}, {} and {} found in {}",
            plural(plugins, "plugin", "plugins"),
            plural(themes, "theme", "themes"),
            plural(maps, "mapping", "mappings"),
            plural(ok, "repository", "repositories")
        ),
    };
    if bad > 0 {
        text += &format!(
            "; {} could not be read",
            plural(bad, "repository", "repositories")
        );
    }
    STATE.with_borrow_mut(|st| {
        *st = State {
            loaded: true,
            repos,
            installed,
            query: std::mem::take(&mut st.query),
            offer: st.offer.take(),
        }
    });
    report(app, &text, bad > 0);
}

fn skin_of(f: &catalog::themes::File) -> Option<Skin> {
    super::skin::get::parse(f)
}

/// ADD: the repository is stored (a typo is refused here), then the list is read again.
fn add(text: &str) {
    let (Some(app), text) = (app(), text.trim().to_owned()) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let added = list::add(&text);
        let _ = weak.upgrade_in_event_loop(move |app| match added {
            Ok(_) => read_all(false),
            Err(e) => say(&app.global::<CatalogState>(), &e, true),
        });
    });
}

/// The x of repository `i`: gone from the list and the cards at once, the file written on a thread.
fn remove(i: i32) {
    let Some(app) = app() else { return };
    let Some(repo) = STATE.with_borrow_mut(|st| {
        let i = usize::try_from(i).ok().filter(|&i| i < st.repos.len())?;
        Some(st.repos.remove(i).0)
    }) else {
        return;
    };
    show(&app);
    say(
        &app.global::<CatalogState>(),
        &format!("Removed {repo}"),
        false,
    );
    let weak = app.as_weak();
    std::thread::spawn(move || {
        if let Err(e) = list::remove(&repo) {
            let _ =
                weak.upgrade_in_event_loop(move |app| say(&app.global::<CatalogState>(), &e, true));
        }
    });
}

/// A page with repositories opened: the first time reads them, later ones show what was read (installed state may have changed).
fn opened() {
    let Some(app) = app() else { return };
    if STATE.with_borrow(|st| st.loaded) {
        show(&app);
    } else {
        read_all(false);
    }
}

/// Connects GET PLUGINS, GET THEMES and GET MAPPINGS; call after `prefs::wire` and `skin::wire`, before the controller session
/// (which asks [`unmapped`] about a controller with no mapping as it opens).
pub fn wire(app: &App) {
    APP.with_borrow_mut(|a| *a = Some(app.as_weak()));
    let s = app.global::<CatalogState>();
    s.on_opened(opened);
    s.on_add(|t: SharedString| add(&t));
    s.on_remove(remove);
    s.on_refresh(|| read_all(true));
    s.on_install_plugin(plugin::install);
    s.on_install_all_plugins(plugin::install_all);
    s.on_install_theme(theme::install);
    s.on_apply_theme(theme::apply);
    s.on_search_mappings(|q: SharedString| mapping::search(&q));
    s.on_install_mapping(mapping::install);
    s.on_install_offer(mapping::install_offer);
    s.on_dismiss_offer(mapping::dismiss_offer);
    prefs::on_change(|_| theme::mark_active());
}
