//! The plugin cards of GET PLUGINS and their installs: one at a time on a worker thread ([`openfader::catalog::plugins::install`]),
//! then the plugin manager reads its folders again ([`plugins::installed`]), which also raises its restart banner.

use super::{CatalogState, STATE, Source, State, app, report, start};
use crate::app::{GetPlugin, plugins};
use openfader::catalog::{self, Repo, plugins::Entry};
use openfader::plugin::current;
use slint::{ComponentHandle, SharedString};
use std::path::PathBuf;

/// Every plugin of the repositories read, in list order (the index a card's button reports).
fn all(st: &State) -> Vec<(&Repo, &Source, &Entry)> {
    st.repos
        .iter()
        .filter_map(|(repo, read)| Some((repo, read.as_ref().ok()?)))
        .flat_map(|(repo, src)| src.plugins.iter().map(move |e| (repo, src, e)))
        .collect()
}

/// The version installed under `name` (switched on or off), if any.
fn installed_version(name: &str) -> Option<String> {
    let now = current();
    now.plugins
        .iter()
        .chain(&now.disabled)
        .find(|p| p.manifest.name == name)
        .map(|p| p.manifest.version.clone())
}

/// 0 INSTALL, 1 UPDATE (another version than the installed one), 2 INSTALLED.
fn action(e: &Entry) -> i32 {
    match installed_version(&e.manifest.name) {
        None => 0,
        Some(v) if v != e.manifest.version => 1,
        Some(_) => 2,
    }
}

pub(super) fn views(st: &State) -> Vec<GetPlugin> {
    all(st)
        .into_iter()
        .map(|(repo, _, e)| {
            let m = &e.manifest;
            GetPlugin {
                name: m.name.as_str().into(),
                version: m.version.as_str().into(),
                installed: installed_version(&m.name).unwrap_or_default().into(),
                description: m.description.as_str().into(),
                repo: repo.to_string().into(),
                native: !catalog::plugins::natives(m).is_empty(),
                action: action(e),
            }
        })
        .collect()
}

type Job = (Repo, PathBuf, Entry);

/// The install job of every plugin in card order, with whether it still needs installing.
fn jobs() -> Vec<(Job, bool)> {
    STATE.with_borrow(|st| {
        all(st)
            .into_iter()
            .map(|(repo, src, e)| ((repo.clone(), src.zip.clone(), e.clone()), action(e) != 2))
            .collect()
    })
}

/// INSTALL / UPDATE of card `i`.
pub(super) fn install(i: i32) {
    let at = usize::try_from(i).unwrap_or(usize::MAX);
    if let Some((job, _)) = jobs().into_iter().nth(at) {
        run(vec![job]);
    }
}

/// INSTALL ALL: every plugin that is not installed or out of date, one after the other.
pub(super) fn install_all() {
    let todo: Vec<Job> = jobs().into_iter().filter(|j| j.1).map(|j| j.0).collect();
    if !todo.is_empty() {
        run(todo);
    }
}

fn run(jobs: Vec<Job>) {
    let Some(app) = app() else { return };
    let s = app.global::<CatalogState>();
    if s.get_busy() {
        return;
    }
    start(&s, &tr!("Installing {}…", jobs[0].2.manifest.name));
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let (mut ok, mut errs) = (Vec::new(), Vec::new());
        for (repo, zip, e) in &jobs {
            let name = e.manifest.name.clone();
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.global::<CatalogState>()
                    .set_status(SharedString::from(tr!("Installing {}…", name)));
            });
            match catalog::plugins::install(repo, zip, e) {
                Ok(name) => ok.push(name),
                Err(e) => errs.push(e),
            }
        }
        let _ = weak.upgrade_in_event_loop(move |app| {
            let mut parts = Vec::new();
            if !ok.is_empty() {
                parts.push(plugins::installed(&app, &ok));
            }
            if !errs.is_empty() {
                parts.push(errs.join("; "));
            }
            let text = parts.join(". ");
            report(&app, &text, !errs.is_empty());
            plugins::say(&app, &text);
        });
    });
}
