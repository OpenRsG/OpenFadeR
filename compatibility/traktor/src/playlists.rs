//! The `PLAYLISTS` tree: the snapshot's folders and playlists merged into the one the file holds.
//!
//! Structure as Traktor writes it (Mixxx `traktorfeature.cpp` `parsePlaylists`; polamjag/traktor-nml demo file): a `$ROOT`
//! folder node whose `SUBNODES` hold `NODE TYPE="FOLDER"` (again with `SUBNODES`) and `NODE TYPE="PLAYLIST"` children; a
//! playlist is `<PLAYLIST ENTRIES TYPE="LIST" UUID>` with one `<ENTRY><PRIMARYKEY TYPE="TRACK" KEY=…/></ENTRY>` per track,
//! `KEY` being the track's `VOLUME + DIR + FILE`.

use openfader_plugin::library::Snapshot;

use crate::xml::El;

fn is(node: &El, kind: &str, name: &str) -> bool {
    node.get("TYPE") == Some(kind) && node.get("NAME") == Some(name)
}

/// The `SUBNODES` of the folder at `path` below `node` (a folder node), the folders missing on the way created.
fn subnodes<'a>(mut node: &'a mut El, path: &[String]) -> &'a mut El {
    for name in path {
        let subs = node.child_mut("SUBNODES");
        let at = subs
            .kids
            .iter()
            .position(|n| is(n, "FOLDER", name))
            .unwrap_or_else(|| {
                subs.kids
                    .push(El::new("NODE").attr("TYPE", "FOLDER").attr("NAME", name));
                subs.kids.len() - 1
            });
        node = &mut subs.kids[at];
    }
    node.child_mut("SUBNODES")
}

/// 32 hex digits, the same for the same `path` (two FNV-1a runs): the `UUID` of a playlist written without one.
fn uuid(path: &str) -> String {
    let fnv = |seed: u64| {
        path.bytes().fold(seed, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        })
    };
    format!(
        "{:016x}{:016x}",
        fnv(0xcbf2_9ce4_8422_2325),
        fnv(0x8422_2325_cbf2_9ce4)
    )
}

/// Entry counts of every `SUBNODES` and `PLAYLIST` below `el`, after the merge changed them.
fn recount(el: &mut El) {
    let n = el.kids.len();
    match el.name.as_str() {
        "SUBNODES" => el.set("COUNT", n),
        "PLAYLIST" => el.set("ENTRIES", n),
        _ => {}
    }
    el.kids.iter_mut().for_each(recount);
}

/// Puts the playlists of `snap` into `nml`: a playlist replaces the one of the same name in the same folder (keeping its
/// place and `UUID`), everything else of the file stays. `keys[i]` is the `PRIMARYKEY` of track `i` of the snapshot, `None`
/// for a track that was not written (it is left out of the playlists). Returns the number of folders and playlists written.
pub fn merge(nml: &mut El, snap: &Snapshot, keys: &[Option<String>]) -> usize {
    let top = nml.child_mut("PLAYLISTS");
    let at = top
        .kids
        .iter()
        .position(|n| is(n, "FOLDER", "$ROOT"))
        .unwrap_or_else(|| {
            top.kids
                .push(El::new("NODE").attr("TYPE", "FOLDER").attr("NAME", "$ROOT"));
            top.kids.len() - 1
        });
    let root = &mut top.kids[at];
    let mut path: Vec<String> = Vec::new();
    for (pl, members) in snap.playlists.iter().zip(&snap.members) {
        path.truncate(pl.depth);
        if pl.is_folder {
            path.push(pl.name.clone());
            subnodes(root, &path);
            continue;
        }
        let subs = subnodes(root, &path);
        let mut list = El::new("PLAYLIST").attr("ENTRIES", 0).attr("TYPE", "LIST");
        let old = subs.kids.iter().position(|n| is(n, "PLAYLIST", &pl.name));
        let id = old
            .and_then(|i| {
                subs.kids[i]
                    .child("PLAYLIST")?
                    .get("UUID")
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| uuid(&format!("{}/{}", path.join("/"), pl.name)));
        list.set("UUID", id);
        for key in members.iter().filter_map(|&i| keys.get(i)?.as_deref()) {
            list.kids.push(
                El::new("ENTRY").with(El::new("PRIMARYKEY").attr("TYPE", "TRACK").attr("KEY", key)),
            );
        }
        let node = El::new("NODE")
            .attr("TYPE", "PLAYLIST")
            .attr("NAME", &pl.name)
            .with(list);
        match old {
            Some(i) => subs.kids[i] = node,
            None => subs.kids.push(node),
        }
    }
    recount(top);
    snap.playlists.len()
}
