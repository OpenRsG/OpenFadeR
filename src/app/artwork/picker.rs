//! Find Artwork: the Track menu / track info action. Every enabled artwork plugin is asked about the chosen track on a thread
//! ([`artwork::search`]), the candidates' thumbnails are downloaded into a cache folder, and the picker (ui/artwork.slint) shows
//! them. A click downloads the full picture, keeps it ([`artwork::save`]: one undo step) and, with Preferences > Advanced >
//! Browse > Write fetched artwork into the file's tags, writes it into the tag of every listed track of the album.

use super::{changed, image as load, plugins, say, App, Ctx};
use crate::app::{browser, library::settings, ArtCandidate, ArtworkState};
use openfader::{
    artwork::{self, image, Candidate, Gate, Query},
    library::Track,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// The thumbnails' side, in pixels.
const THUMB: u32 = 224;

/// What the open picker shows.
struct Picked {
    /// The search it belongs to: an answer of an older one is dropped.
    serial: u32,
    track: Track,
    candidates: Vec<Candidate>,
}

thread_local! {
    static PICKED: RefCell<Option<Picked>> = const { RefCell::new(None) };
    static SERIAL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn thumb_dir() -> Option<PathBuf> {
    artwork::cache_dir().map(|d| d.join("thumbs"))
}

/// Find Artwork on the chosen track: opens the picker and asks the plugins.
pub fn find(c: &Rc<Ctx>) {
    let Some(track) = browser::context().track else {
        return c.say(tr!("choose a track first"));
    };
    let plugins = plugins();
    if plugins.is_empty() {
        return c.say(tr!(
            "no artwork plugin is switched on (Preferences > Plugins: cover-art)"
        ));
    }
    let serial = SERIAL.get().wrapping_add(1);
    SERIAL.set(serial);
    let q = Query::of(&track);
    let app = c.app();
    let s = app.global::<ArtworkState>();
    s.set_title(format!("{} — {}", q.artist, q.album).into());
    s.set_status(tr!("asking {} …", names(&plugins)).into());
    s.set_candidates(ModelRc::default());
    s.set_saving(false);
    s.set_open(true);
    PICKED.with_borrow_mut(|p| {
        *p = Some(Picked {
            serial,
            track,
            candidates: Vec::new(),
        })
    });
    c.spawn(
        move || {
            let found = artwork::search(&plugins, &q, Gate::polite());
            let dir = thumb_dir();
            if let Some(d) = &dir {
                let _ = std::fs::remove_dir_all(d);
            }
            let thumbs: Vec<Option<PathBuf>> = found
                .candidates
                .iter()
                .enumerate()
                .map(|(i, cand)| thumb_file(dir.as_deref()?, i, cand))
                .collect();
            (found, thumbs)
        },
        move |c, (found, thumbs)| shown(c, serial, found, thumbs),
    );
}

fn names(plugins: &[openfader::plugin::Plugin]) -> String {
    plugins
        .iter()
        .map(|p| p.manifest.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The small picture of `c` as a file (the picker loads it), or `None` when it cannot be had.
fn thumb_file(dir: &std::path::Path, i: usize, c: &Candidate) -> Option<PathBuf> {
    let bytes = c.bytes(true).ok()?;
    let kind = image::kind(&bytes)?;
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{i}.{}", kind.ext));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// The search ended: the candidates go into the picker (unless another search or a close came first).
fn shown(c: &Rc<Ctx>, serial: u32, found: artwork::Found, thumbs: Vec<Option<PathBuf>>) {
    let current = PICKED.with_borrow(|p| p.as_ref().is_some_and(|p| p.serial == serial));
    if !current {
        return;
    }
    let rows: Vec<ArtCandidate> = found
        .candidates
        .iter()
        .zip(&thumbs)
        .map(|(cand, file)| ArtCandidate {
            thumb: file
                .as_deref()
                .and_then(|f| load(f, THUMB))
                .unwrap_or_default(),
            source: cand.source.as_str().into(),
            size: if cand.size > 0 {
                tr!("{} px", cand.size).into()
            } else {
                "".into()
            },
        })
        .collect();
    let status = match (rows.is_empty(), found.errors.first()) {
        (false, _) => String::new(),
        (true, Some(e)) => tr!("no artwork: {}", e),
        (true, None) => tr!("no artwork found").to_owned(),
    };
    let app = c.app();
    let s = app.global::<ArtworkState>();
    s.set_status(status.into());
    s.set_candidates(ModelRc::new(VecModel::from(rows)));
    PICKED.with_borrow_mut(|p| {
        if let Some(p) = p {
            p.candidates = found.candidates;
        }
    });
}

/// A candidate was clicked: download it, write it into the tags when asked, keep it.
fn choose(c: &Rc<Ctx>, i: usize) {
    let Some((track, cand)) = PICKED.with_borrow(|p| {
        let p = p.as_ref()?;
        Some((p.track.clone(), p.candidates.get(i)?.clone()))
    }) else {
        return;
    };
    let app = c.app();
    let s = app.global::<ArtworkState>();
    if s.get_saving() {
        return;
    }
    s.set_saving(true);
    s.set_status(tr!("downloading …").into());
    let key = c.store.artwork_key(&track);
    let files: Vec<PathBuf> = if settings::get().artwork_embed && !settings::get().protect {
        browser::context()
            .list
            .iter()
            .filter(|t| c.store.artwork_key(t) == key)
            .filter_map(|t| t.file.clone())
            .collect()
    } else {
        Vec::new()
    };
    c.spawn(
        move || {
            let bytes = cand.bytes(false)?;
            let mut note = None;
            for f in &files {
                if let Err(e) = artwork::embed(f, &bytes) {
                    note.get_or_insert(format!("{}: {e}", f.display()));
                }
            }
            Ok::<_, String>((bytes, note))
        },
        move |c, got| {
            let app = c.app();
            let s = app.global::<ArtworkState>();
            s.set_saving(false);
            let saved = got.and_then(|(bytes, note)| {
                let dir = artwork::cache_dir().ok_or_else(|| "no data folder".to_owned())?;
                artwork::save(&c.store, &dir, &track, &bytes).map(|_| note)
            });
            match saved {
                Ok(note) => {
                    close();
                    c.app().global::<ArtworkState>().set_open(false);
                    changed(c);
                    match note {
                        Some(n) => say(&c.app(), &tr!("cover kept; tag not written: {}", n)),
                        None => say(&c.app(), tr!("cover kept")),
                    }
                }
                Err(e) => s.set_status(e.into()),
            }
        },
    );
}

fn close() {
    PICKED.with_borrow_mut(|p| *p = None);
    if let Some(d) = thumb_dir() {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Connects the picker's callbacks (once, UI thread).
pub fn wire(app: &App, c: &Rc<Ctx>) {
    let s = app.global::<ArtworkState>();
    let k = c.clone();
    s.on_choose(move |i| choose(&k, i as usize));
    let weak = app.as_weak();
    s.on_closed(move || {
        close();
        if let Some(app) = weak.upgrade() {
            app.global::<ArtworkState>().set_open(false);
        }
    });
}
