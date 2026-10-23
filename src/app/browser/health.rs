//! The browser's "Library health" node: one sidebar row per [`Check`] with its number of tracks, and the list of a chosen check in
//! the track table. The scan ([`crate::app::library::health`]) runs on a worker thread; this side only starts it and takes the
//! result.
//!
//! A scan starts [`SETTLE`] after the store, the bulk analysis or the thresholds changed (edits, imports, relocations, removals and
//! finished analyses all do), as an incremental one; choosing the "Library health" row or RESCAN makes the next one full (every file
//! is looked at again). One scan runs at a time: a change that comes while it runs schedules the next. What the user ignored is read
//! from the store when a list is shown, so IGNORE and UN-IGNORE need no scan.
//!
//! The track table's health bar (ui/browser.slint) acts on the chosen row: ANALYSE, RELOCATE, EDIT TAGS and REMOVE are the Track
//! menu's actions, ANALYSE ALL / AUTO RELOCATE ALL / IGNORE ALL act on every listed track, SHOW IGNORED lists what was ignored.

use super::{sidebar::Entry, App, BrowserState, Home, View};
use crate::app::{
    analyze::detected,
    library::{
        health::{shown, Check, Limits, Report, Scanner},
        settings,
    },
    prefs,
};
use openfader::{library::Track, store};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::mpsc::{self, TryRecvError},
    time::{Duration, Instant},
};

/// How long after the first change a scan waits, so a burst of edits or a run of finished analyses is one scan.
const SETTLE: Duration = Duration::from_millis(1500);
/// The first scan, after the window is up.
const FIRST: Duration = Duration::from_secs(2);

/// The scan and what it found.
pub(super) struct Monitor {
    /// The probes of the last scan; `None` while a scan runs.
    scanner: Option<Scanner>,
    running: Option<mpsc::Receiver<(Scanner, Report)>>,
    report: Option<Report>,
    /// Ignored row ids by check id, as of the last result or ignore.
    ignores: HashMap<String, HashSet<i64>>,
    /// `(Store::generation, Detected::version, limits)` as last seen.
    seen: Option<(u64, u64, Limits)>,
    /// When the next scan starts.
    due: Option<Instant>,
    /// The next scan looks at every file again.
    full: bool,
    /// The list shows what was ignored instead.
    ignored: bool,
}

impl Monitor {
    pub(super) fn new() -> Monitor {
        Monitor {
            scanner: Some(Scanner::disk()),
            running: None,
            report: None,
            ignores: HashMap::new(),
            seen: None,
            due: Some(Instant::now() + FIRST),
            full: true,
            ignored: false,
        }
    }

    /// The next scan is a full one, now.
    pub(super) fn rescan(&mut self) {
        self.full = true;
        self.due = Some(Instant::now());
    }

    /// Tracks each check lists, ignores applied.
    fn counts(&self) -> Option<Vec<usize>> {
        let r = self.report.as_ref()?;
        Some(
            Check::ALL
                .iter()
                .map(|c| shown(r.of(*c), self.ignores.get(c.id()), false).len())
                .collect(),
        )
    }
}

/// The sidebar rows of the node: its heading, then one row per check with its count (none until the first scan is done).
fn rows(counts: Option<&[usize]>) -> Vec<(Entry, String)> {
    let mut rows = vec![(Entry::HealthRoot, tr!("▸ Library health").to_owned())];
    rows.extend(Check::ALL.iter().map(|c| {
        let label = match counts.and_then(|n| n.get(c.index())) {
            Some(n) => format!("    {} ({n})", c.label()),
            None => format!("    {}", c.label()),
        };
        (Entry::Health(*c), label)
    }));
    rows
}

/// Puts the node's rows into the sidebar `entries` / `labels` (parallel lists), before the Library node.
fn insert(entries: &mut Vec<Entry>, labels: &mut Vec<String>, counts: Option<&[usize]>) {
    let at = entries
        .iter()
        .position(|e| *e == Entry::LibRoot)
        .unwrap_or(entries.len());
    let (e, l): (Vec<_>, Vec<_>) = rows(counts).into_iter().unzip();
    entries.splice(at..at, e);
    labels.splice(at..at, l);
}

pub(super) fn root_hint() -> &'static str {
    tr!(
        "Library health: choose a row to list the tracks with that problem, then fix them or IGNORE them. Choosing this row scans again"
    )
}

impl View {
    /// Called by the timer: takes a finished scan, and starts one when something changed.
    pub(super) fn health_tick(&mut self, app: &App) {
        let now = Instant::now();
        if let Some(rx) = &self.health.running {
            match rx.try_recv() {
                Ok((scanner, report)) => {
                    self.health.scanner = Some(scanner);
                    self.health.running = None;
                    self.health_report(app, report);
                }
                Err(TryRecvError::Disconnected) => {
                    // The worker died: start over from nothing.
                    self.health.scanner = Some(Scanner::disk());
                    self.health.running = None;
                    self.health.rescan();
                }
                Err(TryRecvError::Empty) => return,
            }
        }
        let key = (
            self.store.generation(),
            detected().version(),
            prefs::with(settings::health_limits),
        );
        if self.health.seen.as_ref() != Some(&key) {
            self.health.seen = Some(key);
            self.health.due.get_or_insert(now + SETTLE);
        }
        if self.health.due.is_some_and(|d| now >= d) {
            self.health_start(key.2);
        }
    }

    fn health_start(&mut self, limits: Limits) {
        let Some(scanner) = self.health.scanner.take() else {
            return;
        };
        self.health.due = None;
        let Ok(cache) = self.cache() else {
            self.health.scanner = Some(scanner);
            return;
        };
        let (tracks, store, full) = (
            cache.tracks.clone(),
            self.store.clone(),
            std::mem::take(&mut self.health.full),
        );
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut scanner = scanner;
            let report = scanner.run(&tracks, &store, &limits, full);
            let _ = tx.send((scanner, report));
        });
        self.health.running = Some(rx);
    }

    /// A scan finished: the sidebar counts follow, and the list when it shows a check whose tracks changed.
    fn health_report(&mut self, app: &App, report: Report) {
        let (counts, old) = (self.health.counts(), self.health.report.take());
        self.health.ignores = self.store.health_ignores().unwrap_or_default();
        let on_screen = match self.home {
            Home::Health(c) if self.related.is_none() => Some(c),
            _ => None,
        };
        let moved = on_screen.is_some_and(|c| old.as_ref().is_none_or(|o| o.of(c) != report.of(c)));
        self.health.report = Some(report);
        if self.health.counts() != counts {
            self.rebuild(app);
        }
        if moved {
            self.load(app);
        }
    }

    /// Puts the node's rows into the sidebar `entries` / `labels` (see [`insert`]).
    pub(super) fn health_insert(&self, entries: &mut Vec<Entry>, labels: &mut Vec<String>) {
        insert(entries, labels, self.health.counts().as_deref());
    }

    /// The tracks of `c`, as the browser lists them now.
    pub(super) fn health_list(&mut self, c: Check) -> Result<Vec<Track>, String> {
        let Some(report) = &self.health.report else {
            return Ok(Vec::new());
        };
        let ids = shown(
            report.of(c),
            self.health.ignores.get(c.id()),
            self.health.ignored,
        );
        let cache = self.cache()?;
        Ok(ids
            .iter()
            .filter_map(|id| cache.by_key.get(&store::id_key(*id)))
            .map(|&i| cache.tracks[i].clone())
            .collect())
    }

    /// The status line of a check's list.
    pub(super) fn health_status(&self, c: Check) -> String {
        let Some(report) = &self.health.report else {
            return tr!("Library health: scanning the Collection…").to_owned();
        };
        let all = report.of(c);
        let ignored = self.health.ignores.get(c.id());
        let n_ignored = all
            .iter()
            .filter(|id| ignored.is_some_and(|s| s.contains(id)))
            .count();
        if self.health.ignored {
            tr!(
                "{}: {} ignored tracks. UN-IGNORE lists a track again",
                c.label(),
                n_ignored
            )
        } else {
            tr!(
                "{}: {} tracks, {} ignored. {}",
                c.label(),
                all.len() - n_ignored,
                n_ignored,
                c.hint(&prefs::with(settings::health_limits))
            )
        }
    }

    /// A track of a check's list was chosen: a corrupt file says why its last load failed.
    pub(super) fn health_selected(&self, app: &App, t: &Track) {
        let reason = match (&self.home, &self.health.report) {
            (Home::Health(Check::Corrupt), Some(r)) => t.id.and_then(|id| r.failures.get(&id)),
            _ => None,
        };
        if let Some(reason) = reason {
            self.say(app, tr!("{}: {}", t.title, reason));
        }
    }

    /// The health bar's own buttons: `ignore` (the chosen track), `ignore-all` (every listed track), `ignored` (show what was
    /// ignored instead), `rescan`.
    fn health_act(&mut self, app: &App, op: &str) {
        let Home::Health(c) = self.home else { return };
        match op {
            "ignored" => {
                self.health.ignored ^= true;
                app.global::<BrowserState>()
                    .set_health_ignored(self.health.ignored);
                self.load(app);
            }
            "rescan" => {
                self.health.rescan();
                self.say(app, tr!("scanning the Collection…"));
            }
            "ignore" | "ignore-all" => {
                let tracks: Vec<&Track> = if op == "ignore" {
                    self.cursor(app)
                        .map(|i| &self.list[i])
                        .into_iter()
                        .collect()
                } else {
                    self.shown.iter().map(|&i| &self.list[i]).collect()
                };
                let ids: Vec<i64> = tracks.iter().filter_map(|t| t.id).collect();
                if ids.is_empty() {
                    return self.say(app, tr!("choose a track first"));
                }
                let on = !self.health.ignored;
                if let Err(e) = self.store.health_ignore(c.id(), &ids, on) {
                    return self.say(app, tr!("store: {}", e));
                }
                self.health.ignores = self.store.health_ignores().unwrap_or_default();
                self.rebuild(app);
                self.load(app);
                self.say(
                    app,
                    if on {
                        tr_n!(
                            "ignored {n} track for “{}”",
                            "ignored {n} tracks for “{}”",
                            ids.len(),
                            c.label()
                        )
                    } else {
                        tr_n!(
                            "{n} track is listed again by “{}”",
                            "{n} tracks are listed again by “{}”",
                            ids.len(),
                            c.label()
                        )
                    },
                );
            }
            _ => {}
        }
    }
}

/// Connects `BrowserState.health-act`.
pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let (v, weak) = (view.clone(), app.as_weak());
    app.global::<BrowserState>()
        .on_health_act(move |op| v.borrow_mut().health_act(&weak.unwrap(), &op));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_node_has_a_heading_and_a_row_per_check_with_its_count_once_scanned() {
        let none = rows(None);
        assert_eq!(none.len(), 1 + Check::ALL.len());
        assert_eq!(none[0].0, Entry::HealthRoot);
        assert_eq!(
            none[1],
            (Entry::Health(Check::Missing), "    Missing file".to_owned())
        );
        let counts: Vec<usize> = (0..Check::ALL.len()).collect();
        let got = rows(Some(&counts));
        assert_eq!(got[3].1, "    Not analysed (2)");
        assert_eq!(got[10].0, Entry::Health(Check::LowBitrate));
        assert_eq!(got[10].1, "    Low bitrate (9)");
    }

    #[test]
    fn the_node_goes_in_before_the_library_node_and_keeps_both_lists_in_step() {
        let mut entries = vec![Entry::All, Entry::TagsRoot, Entry::LibRoot, Entry::Lib(0)];
        let mut labels: Vec<String> = ["All", "Tags", "Library", "Folder"]
            .map(String::from)
            .to_vec();
        insert(&mut entries, &mut labels, None);
        assert_eq!(entries.len(), labels.len());
        assert_eq!(entries[2], Entry::HealthRoot);
        assert_eq!(entries[2 + Check::ALL.len() + 1], Entry::LibRoot);
        assert_eq!(labels[2 + Check::ALL.len() + 1], "Library");
        // Without a Library node it goes last.
        let (mut e, mut l) = (vec![Entry::All], vec!["All".to_owned()]);
        insert(&mut e, &mut l, None);
        assert_eq!(e[1], Entry::HealthRoot);
    }

    #[test]
    fn counts_leave_out_what_was_ignored() {
        let mut m = Monitor::new();
        assert_eq!(m.counts(), None, "no result yet");
        let mut found = vec![Vec::new(); Check::ALL.len()];
        found[Check::Rating.index()] = vec![1, 2, 3];
        m.report = Some(Report {
            found,
            failures: HashMap::new(),
        });
        m.ignores
            .insert(Check::Rating.id().to_owned(), HashSet::from([2, 77]));
        let counts = m.counts().unwrap();
        assert_eq!(counts[Check::Rating.index()], 2);
        assert_eq!(counts[Check::Tags.index()], 0);
    }
}
