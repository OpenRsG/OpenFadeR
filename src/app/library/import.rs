//! File > Import: tracks, a folder, a playlist file (M3U / M3U8 / PLS), a collection file through a library plugin. The reading
//! runs on a thread ([`openfader::library::import`]); here are the dialogs' answers going in and the result coming out
//! (status line, browser reload, analysis of the new files when Auto Analysis is on: files that came with cues are analysed for
//! the waveform and grid only, [`analyze::request_keep_cues`]).

use super::{analyze, prefs, BrowserState, Ctx};
use openfader::{
    library::{
        self,
        import::{self, Report},
    },
    plugin::library::{open_with, readers},
    store::{NodeKind, Store},
};
use slint::{ComponentHandle, Weak};
use std::{
    cell::Cell,
    collections::HashSet,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// Progress as the status line, at most five times a second, from the worker thread.
fn progress(app: Weak<super::App>, what: &'static str) -> impl Fn(usize, usize) {
    let last = Cell::new(Instant::now());
    move |done, total| {
        if done < total && last.get().elapsed() < Duration::from_millis(200) {
            return;
        }
        last.set(Instant::now());
        let app = app.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = app.upgrade() {
                app.global::<BrowserState>()
                    .set_status(tr!("{} {}/{}…", what, done, total).into());
            }
        });
    }
}

/// What every import hands its worker thread: the store, the files already listed, and a progress reporter.
fn job(
    c: &Ctx,
    what: &'static str,
) -> (Arc<Store>, HashSet<PathBuf>, impl Fn(usize, usize) + use<>) {
    (c.store.clone(), c.known(), progress(c.app.clone(), what))
}

fn finished(c: &Rc<Ctx>, r: Result<Report, String>) {
    match r {
        Ok(r) => {
            c.say(&r.text());
            c.reload(r.node);
            // Tracks that came with cues (another library's) are analysed for the waveform only: CUE Analysis would replace them.
            let (keep, todo): (Vec<library::Track>, Vec<library::Track>) = r
                .files
                .into_iter()
                .map(|f| library::Track {
                    file: Some(f),
                    ..Default::default()
                })
                .partition(|t| {
                    c.store
                        .edits(&c.store.key_of(t))
                        .is_ok_and(|e| e.cues.is_some_and(|cues| !cues.is_empty()))
                });
            c.analyze(todo, false);
            if !keep.is_empty() && prefs::with(|p| p.analysis.auto_analysis) {
                analyze::request_keep_cues(&keep);
            }
        }
        Err(e) => c.say(&tr!("import failed: {}", e)),
    }
}

/// Music files and folders.
pub fn files(c: &Rc<Ctx>, paths: Vec<PathBuf>) {
    let (store, known, p) = job(c, tr!("reading tags"));
    c.say(tr!("importing…"));
    c.spawn(move || import::files(&store, &known, &paths, &p), finished);
}

/// A playlist file.
pub fn playlist(c: &Rc<Ctx>, path: PathBuf) {
    let (store, known, p) = job(c, tr!("reading tags"));
    c.say(tr!("importing…"));
    c.spawn(
        move || import::playlist_file(&store, &known, &path, &p),
        finished,
    );
}

/// The files (with their BPM) of the first playlist of `src` that lists any: what a `samples` plugin puts in the sampler.
fn bank(src: &dyn library::Library) -> Result<Vec<(PathBuf, f32)>, String> {
    for p in src.playlists()?.iter().filter(|p| !p.is_folder) {
        let tracks = src.tracks(Some(&p.id))?;
        if !tracks.is_empty() {
            return Ok(tracks
                .into_iter()
                .filter_map(|t| Some((t.file?, t.bpm_x100 as f32 / 100.0)))
                .collect());
        }
    }
    Ok(Vec::new())
}

/// A sampler pack's library with what its samples' own tags say (title, artist, key, length …) and the pack's BPM, which
/// is how a sample joins the Collection.
struct Tagged<'a>(&'a dyn library::Library);

impl library::Library for Tagged<'_> {
    fn kind(&self) -> &str {
        self.0.kind()
    }
    fn playlists(&self) -> Result<Vec<library::Playlist>, String> {
        self.0.playlists()
    }
    fn tracks(&self, playlist: Option<&str>) -> Result<Vec<library::Track>, String> {
        Ok(self
            .0
            .tracks(playlist)?
            .into_iter()
            .map(|t| match &t.file {
                Some(f) => library::Track {
                    bpm_x100: t.bpm_x100,
                    ..library::folder::read_track(f)
                },
                None => t,
            })
            .collect())
    }
    fn analysis(&self, track: &library::Track) -> Option<openfader::analysis::Analysis> {
        self.0.analysis(track)
    }
}

/// Imports a sampler pack: its samples join the Collection with their tags, its top-level folders replace those of an
/// earlier import of the same pack in My Playlists. Returns the report and the sampler's bank.
fn samples(
    store: &Store,
    src: &dyn library::Library,
    known: &HashSet<PathBuf>,
    progress: &dyn Fn(usize, usize),
) -> Result<(Report, Vec<(PathBuf, f32)>), String> {
    let tops: Vec<String> = src
        .playlists()?
        .into_iter()
        .filter(|p| p.depth == 0 && p.is_folder)
        .map(|p| p.name)
        .collect();
    for n in store
        .nodes()?
        .into_iter()
        .filter(|n| n.parent.is_none() && n.kind == NodeKind::Folder && tops.contains(&n.name))
    {
        store.delete_node(n.id)?;
    }
    let report = import::library(
        store,
        &Tagged(src),
        known,
        import::Scope::All { playlists: true },
        progress,
    )?;
    Ok((report, bank(src)?))
}

/// A collection file read by the library plugin `readers()[i]`, with its playlists. A `samples` plugin (a sampler pack) also
/// fills the empty sampler slots with its first list.
pub fn collection(c: &Rc<Ctx>, path: PathBuf, i: usize) {
    let Some(r) = readers().into_iter().nth(i) else {
        return c.say(tr!("that library plugin is not loaded"));
    };
    let (plugin, sampler) = (r.plugin, r.samples);
    let source = path.display().to_string();
    let (store, known, p) = job(c, tr!("importing"));
    c.say(tr!("importing…"));
    c.spawn(
        move || {
            let src = open_with(&plugin, &path)?;
            if sampler {
                let (report, bank) = samples(&store, &src, &known, &p)?;
                return Ok((report, Some(bank)));
            }
            let report = import::library(
                &store,
                &src,
                &known,
                import::Scope::All { playlists: true },
                &p,
            )?;
            Ok((report, None))
        },
        move |c, r: Result<(Report, Option<Vec<(PathBuf, f32)>>), String>| {
            if let Ok((report, _)) = &r {
                super::report::record(c, &source, report);
            }
            match r {
                Ok((report, Some(bank))) => {
                    let n = crate::app::sampler::load_bank(&c.app(), bank);
                    let text = tr!("{} · {} sampler slots loading", report.text(), n);
                    finished(c, Ok(report));
                    c.say(&text);
                }
                Ok((report, None)) => finished(c, Ok(report)),
                Err(e) => finished(c, Err(e)),
            }
        },
    );
}

/// IMPORT TO COLLECTION on the library plugin / Explorer list on screen: its tracks (and, for a playlist, the playlist)
/// join the Collection.
pub fn source(c: &Rc<Ctx>) {
    use crate::app::browser::{self, SourceType};
    let Some((path, kind, list)) = browser::context().source else {
        return c.say(tr!(
            "choose All tracks or a playlist of a library source or Explorer first"
        ));
    };
    let (store, known, p) = job(c, tr!("importing"));
    c.say(tr!("importing…"));
    c.spawn(
        move || {
            let src: Box<dyn library::Library> = match kind {
                SourceType::Plugin(plugin) => Box::new(open_with(&plugin, &path)?),
                SourceType::Folder => Box::new(library::folder::Folder::open(&path)?),
            };
            let scope = match &list {
                Some(id) => import::Scope::List(id),
                None => import::Scope::All { playlists: false },
            };
            import::library(&store, &*src, &known, scope, &p)
        },
        finished,
    );
}
