//! The mapping rows of GET MAPPINGS (PREFERENCES > CONTROLLER), and the offer: when a controller is connected that no mapping file
//! matches ([`unmapped`], called by the controller session), the cached repository listings are searched, on a worker thread, for
//! a mapping whose `<port>` fits its port name (the rule the session itself loads by, [`get::best`]); one is shown as "Mapping for X
//! available" with an INSTALL button ([`install_offer`]). Installing writes the file into `<config>/mappings/` on a worker thread
//! ([`get::install`]), then the controller session opens again with it ([`get::reload_if_connected`]).

use super::{app, plural, report, start, CatalogState, State, STATE};
use crate::app::{midi::get, toast, App, GetMapping};
use openfader::catalog::{self, list, Repo};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::atomic::{AtomicBool, Ordering};

/// Most rows shown at once (the list has a search box; hundreds of rows would only make the page long).
const SHOWN: usize = 60;

/// Whether the network may be asked for a repository that is not cached yet, once per run: a controller offer must not
/// download every time the ports change while offline.
static TRIED: AtomicBool = AtomicBool::new(false);

/// Every mapping of the repositories read, by controller name; a file name is listed once (the first repository's).
fn all(st: &State) -> Vec<(&Repo, &get::Entry)> {
    let mut out: Vec<(&Repo, &get::Entry)> = Vec::new();
    for (repo, read) in &st.repos {
        for e in read.iter().flat_map(|s| &s.mappings) {
            if !out.iter().any(|(_, o)| o.name == e.name) {
                out.push((repo, e));
            }
        }
    }
    out.sort_by_key(|(_, e)| e.controller.to_lowercase());
    out
}

/// Fills the list under the search box: the rows that fit `st.query`, and the line under them; and the offer banner.
pub(super) fn show(s: &CatalogState, st: &State) {
    let all = all(st);
    let q = st.query.trim().to_lowercase();
    let fits = |e: &get::Entry| {
        [&e.controller, &e.port, &e.name]
            .iter()
            .any(|f| f.to_lowercase().contains(&q))
    };
    let found: Vec<(usize, &Repo, &get::Entry)> = all
        .iter()
        .enumerate()
        .filter(|(_, (_, e))| fits(e))
        .map(|(id, (r, e))| (id, *r, *e))
        .collect();
    let rows: Vec<GetMapping> = found
        .iter()
        .take(SHOWN)
        .map(|&(id, repo, e)| GetMapping {
            id: id as i32,
            controller: e.controller.as_str().into(),
            port: e.port.as_str().into(),
            repo: repo.to_string().into(),
            action: match get::action(e, &st.installed) {
                get::Action::Install => 0,
                get::Action::Update => 1,
                get::Action::Installed => 2,
            },
        })
        .collect();
    s.set_mappings(ModelRc::new(VecModel::from(rows)));
    let note = if found.len() > SHOWN {
        format!(
            "Showing {SHOWN} of {}: type to narrow the list",
            found.len()
        )
    } else if all.is_empty() {
        "No mapping in your repositories".to_owned()
    } else if found.is_empty() {
        "No mapping fits the search".to_owned()
    } else {
        plural(found.len(), "mapping", "mappings")
    };
    s.set_mappings_note(note.into());
    s.set_offer(
        st.offer
            .as_ref()
            .map(|e| format!("Mapping for {} available", e.controller))
            .unwrap_or_default()
            .into(),
    );
}

/// The search box changed.
pub(super) fn search(q: &str) {
    let Some(app) = app() else { return };
    STATE.with_borrow_mut(|st| st.query = q.to_owned());
    STATE.with_borrow(|st| show(&app.global::<CatalogState>(), st));
}

/// INSTALL / UPDATE of the row with `id`.
pub(super) fn install(id: i32) {
    let entry = usize::try_from(id)
        .ok()
        .and_then(|i| STATE.with_borrow(|st| all(st).get(i).map(|(_, e)| (*e).clone())));
    if let Some(e) = entry {
        run(e);
    }
}

/// INSTALL on the banner.
pub(super) fn install_offer() {
    if let Some(e) = STATE.with_borrow(|st| st.offer.clone()) {
        run(e);
    }
}

/// The x on the banner.
pub(super) fn dismiss_offer() {
    clear();
}

fn clear() {
    let Some(app) = app() else { return };
    STATE.with_borrow_mut(|st| st.offer = None);
    app.global::<CatalogState>().set_offer("".into());
}

fn run(e: get::Entry) {
    let Some(app) = app() else { return };
    let s = app.global::<CatalogState>();
    if s.get_busy() {
        return;
    }
    start(&s, &format!("Installing the mapping for {}…", e.controller));
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let done = get::install(&e).map(|()| get::installed());
        let _ = weak.upgrade_in_event_loop(move |app| match done {
            Ok(installed) => {
                STATE.with_borrow_mut(|st| {
                    st.installed = installed;
                    if st.offer.as_ref().is_some_and(|o| o.name == e.name) {
                        st.offer = None;
                    }
                });
                let text = format!("mapping for {} installed", e.controller);
                report(&app, &text, false);
                toast::show(&app, &text);
                get::reload_if_connected(&e);
            }
            Err(why) => {
                report(&app, &why, true);
                toast::show(&app, &why);
            }
        });
    });
}

/// The controller session found input ports but no mapping for them: look for one in the repositories.
pub fn unmapped(mut ports: Vec<String>) {
    // ALSA's always-present "Midi Through" is no controller.
    ports.retain(|p| !p.to_lowercase().contains("midi through"));
    let Some(app) = app() else { return };
    if ports.is_empty() {
        return clear();
    }
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let online = !TRIED.swap(true, Ordering::Relaxed);
        let mut entries = Vec::new();
        for repo in list::load() {
            let zip = catalog::cached(&repo)
                .or_else(|| online.then(|| catalog::fetch(&repo, false).ok()).flatten());
            if let Some(zip) = zip {
                entries.extend(
                    catalog::mappings::read(&zip)
                        .iter()
                        .filter_map(|f| get::entry(f).ok()),
                );
            }
        }
        let Some(best) = get::best(&ports, &entries).cloned() else {
            return;
        };
        let _ = weak.upgrade_in_event_loop(move |app| offered(&app, best));
    });
}

/// A mapping fits a connected controller: show the banner (and say so once).
fn offered(app: &App, e: get::Entry) {
    let same = STATE.with_borrow(|st| st.offer.as_ref().is_some_and(|o| o.name == e.name));
    let text = format!("Mapping for {} available", e.controller);
    STATE.with_borrow_mut(|st| st.offer = Some(e));
    app.global::<CatalogState>().set_offer(text.as_str().into());
    if !same {
        toast::show(app, &format!("{text}: INSTALL it (top right)"));
    }
}

/// A controller is connected with its mapping: no offer.
pub fn mapped() {
    if STATE.with_borrow(|st| st.offer.is_some()) {
        clear();
    }
}
