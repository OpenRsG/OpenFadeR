//! The Playlist menu: New Playlist / Folder / Intelligent Playlist, Delete Playlist, Sort Items, Renumber
//! Track Order (those four act on the playlist tree and are done by the browser, [`BrowserState::playlist_op`]),
//! Export a playlist to a file (M3U8, PLS, text) and Import Playlist (M3U / M3U8 / PLS).

use super::{Ask, BrowserState, Ctx};
use crate::app::{DeviceState, browser, plural};
use openfader::{
    library::playlist_file::{self, Format},
    paths,
};
use slint::ComponentHandle;
use std::{path::PathBuf, rc::Rc};

pub fn act(c: &Rc<Ctx>, id: &str) {
    let format = match id {
        "playlist-export-m3u8" => Some(Format::M3u8),
        "playlist-export-pls" => Some(Format::Pls),
        "playlist-export-text" => Some(Format::Text),
        _ => None,
    };
    if let Some(f) = format {
        let ctx = browser::context();
        if ctx.list.is_empty() {
            return c.say("this list is empty: choose a playlist with tracks first");
        }
        let name: String = ctx
            .name
            .chars()
            .map(|ch| {
                if matches!(ch, '/' | '\\' | ':') {
                    '-'
                } else {
                    ch
                }
            })
            .collect();
        let initial = paths::output_dir("OPENFADER_EXPORT_DIR").map(|d| {
            d.join(format!("{name}.{}", f.extension()))
                .display()
                .to_string()
        });
        return c.ask_path(Ask::ExportList(f), initial);
    }
    match id {
        "playlist-import" => c.ask_path(Ask::ImportPlaylist, None),
        "playlist-export-device" => match browser::context().playlist {
            Some(id) => c
                .app()
                .global::<DeviceState>()
                .invoke_export_playlists(id.into()),
            None => c.say("choose one of your playlists or a playlist of the library first"),
        },
        _ => c
            .app()
            .global::<BrowserState>()
            .invoke_playlist_op(id.trim_start_matches("playlist-").into()),
    }
}

/// Export a playlist to a file: the list on screen goes to `path`.
pub fn write(c: &Rc<Ctx>, path: PathBuf, format: Format) {
    let list = browser::context().list;
    match playlist_file::write(&path, &list, format) {
        Ok(n) => c.say(&format!(
            "exported {n} track{} to {}",
            plural(n),
            path.display()
        )),
        Err(e) => c.say(&format!("export failed: {e}")),
    }
}
