//! Turning the flat folder / playlist rows most formats store into the depth-first [`Playlist`] list a library answers with.

use super::Playlist;
use std::collections::{HashMap, HashSet};

/// Folders nested deeper than this are not listed.
pub const MAX_DEPTH: usize = 64;

/// One flat playlist/folder row as a source stores it.
#[derive(Clone)]
pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub is_folder: bool,
    /// `None` or an id that is not a known folder: top level.
    pub parent: Option<String>,
    /// Sort key among siblings.
    pub order: i64,
}

/// Depth-first tree, siblings by `order` (stable). Rows whose parent chain never reaches the top (cycles) and folders
/// deeper than [`MAX_DEPTH`] are dropped.
pub fn playlist_tree(rows: &[PlaylistRow]) -> Vec<Playlist> {
    let mut sorted: Vec<&PlaylistRow> = rows.iter().collect();
    sorted.sort_by_key(|r| r.order);
    let folders: HashSet<&str> = rows
        .iter()
        .filter(|r| r.is_folder)
        .map(|r| r.id.as_str())
        .collect();
    let mut kids: HashMap<Option<&str>, Vec<&PlaylistRow>> = HashMap::new();
    for r in sorted {
        kids.entry(r.parent.as_deref().filter(|p| folders.contains(p)))
            .or_default()
            .push(r);
    }
    let mut stack: Vec<(&PlaylistRow, usize)> = kids
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|r| (*r, 0))
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    while let Some((r, depth)) = stack.pop() {
        out.push(Playlist {
            id: r.id.clone(),
            name: r.name.clone(),
            depth,
            is_folder: r.is_folder,
        });
        if r.is_folder && depth + 1 < MAX_DEPTH {
            stack.extend(
                kids.get(&Some(r.id.as_str()))
                    .into_iter()
                    .flatten()
                    .rev()
                    .map(|k| (*k, depth + 1)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, folder: bool, parent: Option<&str>, order: i64) -> PlaylistRow {
        PlaylistRow {
            id: id.into(),
            name: id.into(),
            is_folder: folder,
            parent: parent.map(Into::into),
            order,
        }
    }

    #[test]
    fn tree_order_orphans_cycles_and_depth() {
        let rows = [
            row("b", false, None, 1),
            row("f", true, Some("root"), 0), // parent is not a known folder: top level
            row("y", false, Some("f"), 2),
            row("x", false, Some("f"), 1),
            row("p", false, Some("b"), 0), // parent is a playlist, not a folder: top level
            row("c1", true, Some("c2"), 0), // folder cycle, unreachable
            row("c2", true, Some("c1"), 0),
        ];
        let got: Vec<_> = playlist_tree(&rows)
            .iter()
            .map(|p| (p.id.clone(), p.depth))
            .collect();
        let want = [("f", 0), ("x", 1), ("y", 1), ("p", 0), ("b", 0)];
        assert_eq!(got, want.map(|(i, d)| (i.to_owned(), d)));

        // A chain of folders deeper than MAX_DEPTH is cut off.
        let chain: Vec<_> = (0..MAX_DEPTH + 5)
            .map(|i| {
                row(
                    &i.to_string(),
                    true,
                    i.checked_sub(1).map(|p| p.to_string()).as_deref(),
                    0,
                )
            })
            .collect();
        assert_eq!(playlist_tree(&chain).len(), MAX_DEPTH);
    }
}
