//! The playlists of the stick as `.vdjfolder` files, one per snapshot playlist, a folder of the snapshot being a folder on the
//! stick.
//!
//! Sources: VirtualDJ 2024 and later keep every list as `<drive>:\VirtualDJ\MyLists\<name>.vdjfolder`, VirtualDJ 8 to 2023 keep
//! virtual folders as `.vdjfolder` below `<drive>:\VirtualDJ\Folders` and read the ones of a drive when it is plugged in, a
//! virtual folder inside another one being a subfolder (Stephane Clavel, Phantom Deejay: virtualdj.com forum threads 257603,
//! 244461 and 242242). The file is `<VirtualFolder noDuplicates ordered>` with `<song path size artist title idx/>`
//! (https://virtualdj.com/wiki/lists.html). Inferred: a stick that has a `Folders` folder and no `MyLists` folder was last
//! used with VirtualDJ 2023 or older, so lists go where that version reads them.

use crate::xml::El;
use openfader_plugin::library::{Snapshot, clean_name};
use std::path::{Path, PathBuf};

/// Where the lists of the stick at `root` go: `MyLists`, unless the stick has only the older `Folders`.
pub fn dir(root: &Path) -> PathBuf {
    let vdj = root.join("VirtualDJ");
    if vdj.join("Folders").is_dir() && !vdj.join("MyLists").is_dir() {
        vdj.join("Folders")
    } else {
        vdj.join("MyLists")
    }
}

/// The folders and files of `snapshot` below `base`, and how many songs each file holds. `tracks[i]`: `FilePath` and size of
/// track `i`, `None` for a track that is not on the stick. A warning for two lists of one folder that would share a file.
pub fn build(
    snapshot: &Snapshot,
    base: &Path,
    tracks: &[Option<(String, u64)>],
    warnings: &mut Vec<String>,
) -> (Vec<PathBuf>, Vec<(PathBuf, String)>, Vec<usize>) {
    let (mut dirs, mut files) = (Vec::new(), Vec::<(PathBuf, String)>::new());
    let mut counts = Vec::<usize>::new();
    if !snapshot.playlists.is_empty() {
        dirs.push(base.to_owned());
    }
    let mut stack: Vec<String> = Vec::new();
    for (i, pl) in snapshot.playlists.iter().enumerate() {
        stack.truncate(pl.depth);
        let dir = stack.iter().fold(base.to_owned(), |p, s| p.join(s));
        let name = clean_name(&pl.name, "Playlist");
        if pl.is_folder {
            dirs.push(dir.join(&name));
            stack.push(name);
            continue;
        }
        let mut list = El::new("VirtualFolder");
        list.set("noDuplicates", "no");
        list.set("ordered", "yes");
        let members = snapshot.members.get(i).map_or(&[][..], Vec::as_slice);
        let found = members.iter().filter_map(|&t| {
            let file = tracks.get(t)?.as_ref()?;
            Some((&snapshot.tracks[t].0, file))
        });
        for (idx, (track, (path, size))) in found.enumerate() {
            let mut song = El::new("song");
            song.set("path", path.as_str());
            song.set("size", size.to_string());
            song.set("artist", track.artist.as_str());
            song.set("title", track.title.as_str());
            song.set("idx", idx.to_string());
            list.kids.push(crate::xml::Node::El(song));
        }
        let mut text = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        list.write(0, &mut text);
        text.push('\n');
        let file = dir.join(format!("{name}.vdjfolder"));
        if let Some(at) = files.iter().position(|(f, _)| *f == file) {
            warnings.push(format!(
                "{}: another list of the same folder has this file name, it replaces that one",
                pl.name
            ));
            files.remove(at);
            counts.remove(at);
        }
        counts.push(list.kids.len());
        files.push((file, text));
    }
    (dirs, files, counts)
}
