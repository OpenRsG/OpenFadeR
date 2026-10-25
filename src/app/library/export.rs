//! File > Export (and the browser's EXPORT button): the whole Collection as a file, in the shared output folder ([`paths`]).
//! The choices are the library plugins that write a collection ([`openfader::plugin::library::writers`]; the menu labels come
//! from the plugins' manifests) followed by our own open formats ([`BUILTIN`], `docs/library-format.md`). A plugin's format
//! cannot hold everything: the Collection is fitted to its capabilities first ([`translate::fit`]) and the status line says
//! what did not fit. It carries the store's edits (the overlaid library: rating, comment, cues, grid; the store's colours, tags
//! and user playlists via [`Snapshot::add_store`]) and tracks loaded in a deck carry the deck's session edits instead.
//!
//! `Rc<dyn Library>` cannot leave the UI thread, so the press snapshots the library there ([`Snapshot::take_pending`]: tracks and
//! playlists); the analyses are read and the file written on a thread.

use super::{BrowserState, Ctx, LibraryState};
use crate::app::App;
use openfader::{
    library::{
        open_format,
        snapshot::Snapshot,
        translate::{self, Loss},
    },
    paths,
    plugin::library::{capabilities_of, extension, write_collection, writers},
    store,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{path::Path, rc::Rc};

/// A format of our own: menu label, file extension, writer (returns the number of tracks written).
struct Builtin {
    label: &'static str,
    ext: &'static str,
    write: fn(&Path, &Snapshot) -> Result<usize, String>,
}

const BUILTIN: [Builtin; 3] = [
    Builtin {
        label: n_!("OpenFadeR JSON (.json)"),
        ext: "json",
        write: open_format::write_json,
    },
    Builtin {
        label: n_!("CSV table (.csv)"),
        ext: "csv",
        write: open_format::write_csv,
    },
    Builtin {
        label: n_!("M3U8 playlist (.m3u8)"),
        ext: "m3u8",
        write: open_format::write_m3u8,
    },
];

/// Where an export goes.
enum Target {
    Plugin(String),
    Builtin(&'static Builtin),
}

/// `<$OPENFADER_EXPORT_DIR | …>/<plugin | openfader>-YYYYmmdd-HHMMSS.<extension>` (see [`paths`]), written by the target, removed
/// again when it fails. The message says what a plugin's format could not keep.
fn write(target: &Target, snap: &Snapshot) -> Result<String, String> {
    let dir =
        paths::output_dir("OPENFADER_EXPORT_DIR").ok_or(tr!("set OPENFADER_EXPORT_DIR or HOME"))?;
    let (prefix, ext) = match target {
        Target::Plugin(p) => (
            p.as_str(),
            extension(p).ok_or_else(|| tr!("{}: writes no collection", p))?,
        ),
        Target::Builtin(b) => ("openfader", b.ext.to_owned()),
    };
    let path = paths::at(paths::timestamped(&dir, prefix, &ext), &dir)?;
    let (n, loss) = match target {
        Target::Plugin(p) => {
            let mut wire = snap.to_wire();
            let loss = translate::fit(&mut wire, &capabilities_of(p));
            (write_collection(p, &path, &wire)?, loss)
        }
        Target::Builtin(b) => ((b.write)(&path, snap)?, Loss::default()),
    };
    let mut msg = tr!("exported {} tracks to {}", n, path.display());
    if !loss.is_empty() {
        msg.push_str(&tr!(" · not kept by this format: {}", loss));
    }
    Ok(msg)
}

/// Exports with the `i`th choice: the collection-writing plugins, then [`BUILTIN`].
pub fn collection(c: &Rc<Ctx>, i: usize) {
    let plugins = writers();
    let target = match plugins.get(i) {
        Some(w) => Target::Plugin(w.plugin.clone()),
        None => match BUILTIN.get(i - plugins.len()) {
            Some(b) => Target::Builtin(b),
            None => return c.say(tr!("no such export format")),
        },
    };
    let mut snap = match Snapshot::take_pending(&*c.lib)
        .and_then(|mut s| s.add_store(&store::global()).map(|()| s))
    {
        Ok(s) => s,
        Err(e) => return c.say(&tr!("export failed: {}", e)),
    };
    for (t, beats, cues) in c.decks.iter().filter_map(|d| d.session()) {
        snap.override_session(&t, beats, cues);
    }
    c.say(&tr!("exporting {} tracks…", snap.outline().len()));
    c.spawn(
        move || write(&target, &snap.read(&store::global())),
        |c, r| c.say(&r.unwrap_or_else(|e| tr!("export failed: {}", e))),
    );
}

/// The File menus' entries for the plugins that read / write a collection file plus our own formats, and the browser's EXPORT
/// button (the first choice).
pub fn wire(app: &App, c: &Rc<Ctx>) {
    labels(app);
    let k = c.clone();
    app.global::<BrowserState>()
        .on_export_collection(move || collection(&k, 0));
}

/// The File menus' import / export entries, in the UI language (at start and after a language change).
pub fn labels(app: &App) {
    let labels = |v: Vec<String>| {
        ModelRc::new(VecModel::from(
            v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        ))
    };
    let s = app.global::<LibraryState>();
    s.set_import_readers(labels(
        openfader::plugin::library::readers()
            .into_iter()
            .map(|e| e.label)
            .collect(),
    ));
    s.set_export_writers(labels(
        writers()
            .into_iter()
            .map(|e| e.label)
            .chain(
                BUILTIN
                    .iter()
                    .map(|b| crate::app::i18n::tr_dyn(b.label).to_owned()),
            )
            .collect(),
    ));
}
