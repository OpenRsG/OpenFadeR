//! File > Export (and the browser's EXPORT button): the whole Collection as a file a library plugin writes in its format
//! ([`openfader::plugin::library::writers`]; the menu labels come from the plugins' manifests), in the shared output folder
//! ([`paths`]). It carries the store's edits (the overlaid library: rating, comment, cues, grid; the store's colours, tags
//! and user playlists via [`Snapshot::add_store`]) and tracks loaded in a deck carry the deck's session edits instead.
//!
//! `Rc<dyn Library>` cannot leave the UI thread, so the press snapshots the library there ([`Snapshot::take`]); the plugin
//! writes the file on a thread.

use super::{BrowserState, Ctx, LibraryState};
use crate::app::App;
use openfader::{
    library::snapshot::Snapshot,
    paths,
    plugin::library::{extension, write_collection, writers},
    store,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

/// `<$OPENFADER_EXPORT_DIR | …>/<plugin>-YYYYmmdd-HHMMSS.<extension>` (see [`paths`]), written by `plugin`, removed again
/// when it fails.
fn write(plugin: &str, snap: &Snapshot) -> Result<String, String> {
    let dir =
        paths::output_dir("OPENFADER_EXPORT_DIR").ok_or("set OPENFADER_EXPORT_DIR or HOME")?;
    let ext = extension(plugin).ok_or_else(|| format!("{plugin}: writes no collection"))?;
    let path = paths::at(paths::timestamped(&dir, plugin, &ext), &dir)?;
    write_collection(plugin, &path, &snap.to_wire())
        .map(|n| format!("exported {n} tracks to {}", path.display()))
}

/// Exports with the `i`th collection-writing plugin.
pub fn collection(c: &Rc<Ctx>, i: usize) {
    let Some(plugin) = writers().into_iter().nth(i).map(|w| w.plugin) else {
        return c.say("no library plugin writes a collection (see Preferences > Extensions)");
    };
    let mut snap =
        match Snapshot::take(&*c.lib).and_then(|mut s| s.add_store(&store::global()).map(|()| s)) {
            Ok(s) => s,
            Err(e) => return c.say(&format!("export failed: {e}")),
        };
    for (t, beats, cues) in c.decks.iter().filter_map(|d| d.session()) {
        snap.override_session(&t, beats, cues);
    }
    c.say(&format!("exporting {} tracks…", snap.len()));
    c.spawn(
        move || write(&plugin, &snap),
        |c, r| c.say(&r.unwrap_or_else(|e| format!("export failed: {e}"))),
    );
}

/// The File menus' entries for the plugins that read / write a collection file, and the browser's EXPORT button (the first
/// writer).
pub fn wire(app: &App, c: &Rc<Ctx>) {
    let labels = |v: Vec<openfader::plugin::library::Entry>| {
        ModelRc::new(VecModel::from(
            v.into_iter()
                .map(|e| SharedString::from(e.label))
                .collect::<Vec<_>>(),
        ))
    };
    let s = app.global::<LibraryState>();
    s.set_import_readers(labels(openfader::plugin::library::readers()));
    s.set_export_writers(labels(writers()));
    let k = c.clone();
    app.global::<BrowserState>()
        .on_export_collection(move || collection(&k, 0));
}
