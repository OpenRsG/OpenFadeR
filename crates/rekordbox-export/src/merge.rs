//! Putting a [`Snapshot`]'s playlists and MyTag into a device library that already holds some ([`Db`], [`Tags`]), without IO.
//!
//! Identity (inferred; rekordbox's own rule is not documented): a playlist or folder of the snapshot is the device's one with the
//! same parent, name and kind. A folder is only walked; a playlist gets the snapshot's tracks as its content, in order ("exported in
//! the latest state", spec p1629); anything else on the device stays. Everything new is appended after its siblings.

use super::pdb::db::{Node, Tags};
use openfader_plugin::library::Snapshot;
use std::collections::HashSet;

/// rekordbox refuses more than this many lists and folders on one device (observed string: "The number of lists and folders
/// cannot exceed 1000 on one USB device").
pub(super) const MAX_LISTS: usize = 1000;

/// Places the snapshot's playlists into `nodes`; `ids[i]` is the device id of snapshot track `i` (`None` = left out). Returns how
/// many nodes were appended.
pub(super) fn playlists(nodes: &mut Vec<Node>, snap: &Snapshot, ids: &[Option<u32>]) -> usize {
    let (before, mut next) = (
        nodes.len(),
        nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1,
    );
    let mut open: Vec<(usize, u32)> = Vec::new(); // (depth, device id) of the folders enclosing the current row
    for (p, members) in snap.playlists.iter().zip(&snap.members) {
        while open.last().is_some_and(|f| f.0 >= p.depth) {
            open.pop();
        }
        let parent = open.last().map_or(0, |f| f.1);
        let tracks: Vec<u32> = if p.is_folder {
            Vec::new()
        } else {
            members.iter().filter_map(|&m| ids[m]).collect()
        };
        let id = match nodes
            .iter_mut()
            .find(|n| n.parent == parent && n.name == p.name && n.is_folder == p.is_folder)
        {
            Some(n) => {
                if !p.is_folder {
                    n.tracks = tracks;
                }
                n.id
            }
            None => {
                nodes.push(Node {
                    id: next,
                    parent,
                    name: p.name.clone(),
                    is_folder: p.is_folder,
                    tracks,
                });
                next += 1;
                next - 1
            }
        };
        if p.is_folder {
            open.push((p.depth, id));
        }
    }
    nodes.len() - before
}

/// Merges the snapshot's MyTag (categories and tags by name) into `tags`; the tracks the snapshot exports (`ids`) get exactly the
/// snapshot's tags. A snapshot without MyTag leaves the device's alone.
pub(super) fn mytag(tags: &mut Tags, snap: &Snapshot, ids: &[Option<u32>]) {
    if snap.mytags.is_empty() {
        return;
    }
    let mut at: Vec<Vec<(usize, usize)>> = Vec::new(); // snapshot (category, tag) → device (category, tag)
    for (name, list) in &snap.mytags {
        let c = tags
            .categories
            .iter()
            .position(|(n, _)| n == name)
            .unwrap_or_else(|| {
                tags.categories.push((name.clone(), Vec::new()));
                tags.categories.len() - 1
            });
        let mut row = Vec::new();
        for t in list {
            let i = tags.categories[c]
                .1
                .iter()
                .position(|x| x == t)
                .unwrap_or_else(|| {
                    tags.categories[c].1.push(t.clone());
                    tags.categories[c].1.len() - 1
                });
            row.push((c, i));
        }
        at.push(row);
    }
    let exported: HashSet<u32> = ids.iter().flatten().copied().collect();
    tags.assigned.retain(|(t, ..)| !exported.contains(t));
    for &(track, c, t) in &snap.tagged {
        if let (Some(id), Some(&(dc, dt))) = (ids[track], at.get(c).and_then(|r| r.get(t))) {
            tags.assigned.push((id, dc, dt));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader_plugin::library::{Playlist, Track};

    fn snapshot() -> Snapshot {
        let p = |id: &str, name: &str, depth, is_folder| Playlist {
            id: id.into(),
            name: name.into(),
            depth,
            is_folder,
        };
        let t = |n: &str| {
            (
                Track {
                    title: n.into(),
                    ..Default::default()
                },
                None,
            )
        };
        Snapshot {
            tracks: vec![t("a"), t("b")],
            playlists: vec![
                p("f", "Sets", 0, true),
                p("w", "Warm", 1, false),
                p("t", "Top", 0, false),
            ],
            members: vec![vec![], vec![1, 0], vec![0]],
            ..Default::default()
        }
    }

    #[test]
    fn same_named_playlists_are_replaced_and_new_ones_appended() {
        let snap = snapshot();
        let ids = [Some(10), Some(11)];
        let mut nodes = vec![
            Node {
                id: 5,
                parent: 0,
                name: "Sets".into(),
                is_folder: true,
                tracks: vec![],
            },
            Node {
                id: 6,
                parent: 5,
                name: "Warm".into(),
                is_folder: false,
                tracks: vec![99],
            },
            Node {
                id: 7,
                parent: 0,
                name: "Mine".into(),
                is_folder: false,
                tracks: vec![99],
            },
        ];
        assert_eq!(playlists(&mut nodes, &snap, &ids), 1);
        let shape: Vec<_> = nodes
            .iter()
            .map(|n| (n.id, n.parent, n.name.as_str(), n.tracks.clone()))
            .collect();
        assert_eq!(
            shape,
            [
                (5, 0, "Sets", vec![]),
                (6, 5, "Warm", vec![11, 10]),
                (7, 0, "Mine", vec![99]),
                (8, 0, "Top", vec![10])
            ]
        );
        assert_eq!(playlists(&mut nodes, &snap, &ids), 0, "idempotent");
    }

    #[test]
    fn mytag_merges_by_name_and_replaces_the_exported_tracks_tags() {
        let snap = Snapshot {
            mytags: vec![("Mood".into(), vec!["Dark".into()])],
            tagged: vec![(0, 0, 0)],
            ..snapshot()
        };
        let mut tags = Tags {
            categories: vec![("Mood".into(), vec!["Bright".into()])],
            assigned: vec![(10, 0, 0), (50, 0, 0)],
        };
        mytag(&mut tags, &snap, &[Some(10), None]);
        assert_eq!(
            tags.categories,
            [(
                "Mood".to_string(),
                vec!["Bright".to_string(), "Dark".to_string()]
            )]
        );
        assert_eq!(tags.assigned, [(50, 0, 0), (10, 0, 1)]);
    }
}
