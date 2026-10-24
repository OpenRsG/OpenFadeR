//! Snapshot additions shared by the collection and the device export: what only our store has (track colours, tags, user
//! playlists) and narrowing a snapshot to one playlist. Rating, comment, cues and grid are not here: they reach the snapshot
//! through [`crate::store::Overlay`] when the snapshotted library is the overlaid one.

use super::Snapshot;
use crate::library::Playlist;
use crate::store::{self, NodeKind, Store};
use std::collections::HashMap;

impl Snapshot {
    /// Adds the store's track colours and tag assignments, and its user playlists (after the library's own, top level
    /// first; smart playlists are skipped: their criteria are the browser's). Entries naming tracks that are not in the
    /// snapshot are dropped. A track is matched to the store by [`store::key`].
    pub fn add_store(&mut self, store: &Store) -> Result<(), String> {
        self.add_store_with(store, true)
    }

    /// [`Snapshot::add_store`] without the user playlists: the colours and tags of the tracks only (a transfer of tracks makes no
    /// playlist on the device).
    pub fn add_store_marks(&mut self, store: &Store) -> Result<(), String> {
        self.add_store_with(store, false)
    }

    fn add_store_with(&mut self, store: &Store, playlists: bool) -> Result<(), String> {
        let keys: Vec<String> = self.tracks.iter().map(|(t, _)| store::key(t)).collect();
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, k) in keys.iter().enumerate() {
            index.entry(k).or_insert(i);
        }
        let colors = store.all_track_colors()?;
        self.colors = keys
            .iter()
            .enumerate()
            .filter_map(|(i, k)| colors.get(k).map(|&c| (i, c)))
            .collect();

        let (cats, tags) = (store.mytag_categories()?, store.mytags()?);
        let mut at: HashMap<i64, (usize, usize)> = HashMap::new();
        self.mytags = cats
            .iter()
            .enumerate()
            .map(|(c, cat)| {
                let names: Vec<String> = tags
                    .iter()
                    .filter(|t| t.category == cat.id)
                    .map(|t| t.name.clone())
                    .collect();
                at.extend(
                    tags.iter()
                        .filter(|t| t.category == cat.id)
                        .enumerate()
                        .map(|(j, t)| (t.id, (c, j))),
                );
                (cat.name.clone(), names)
            })
            .collect();
        let tagged = store.all_track_mytags()?;
        self.tagged = keys
            .iter()
            .enumerate()
            .flat_map(|(i, k)| {
                tagged
                    .get(k)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| at.get(t).map(|&(c, j)| (i, c, j)))
                    .collect::<Vec<_>>()
            })
            .collect();

        if !playlists {
            return Ok(());
        }
        let mut depth: HashMap<i64, usize> = HashMap::new();
        for n in store
            .nodes()?
            .into_iter()
            .filter(|n| n.kind != NodeKind::Smart)
        {
            let d = n.parent.and_then(|p| depth.get(&p)).map_or(0, |d| d + 1);
            depth.insert(n.id, d);
            let folder = n.kind == NodeKind::Folder;
            self.members.push(if folder {
                Vec::new()
            } else {
                store
                    .entries(n.id)?
                    .iter()
                    .filter_map(|k| index.get(k.as_str()).copied())
                    .collect()
            });
            self.pls.push(Playlist {
                id: format!("store:{}", n.id),
                name: n.name,
                depth: d,
                is_folder: folder,
            });
        }
        Ok(())
    }

    /// The snapshot narrowed to the playlists with these ids (a folder brings everything under it, a playlist its folders so
    /// the tree keeps its shape); the tracks kept are the ones they list, in order of first use. Only what is kept is cloned.
    /// `None` when no playlist has any of the ids.
    pub fn narrowed(&self, ids: &[&str]) -> Option<Snapshot> {
        let n = self.pls.len();
        let mut keep = vec![false; n];
        for i in 0..n {
            if ids.contains(&self.pls[i].id.as_str()) {
                let depth = self.pls[i].depth;
                keep[i] = true;
                keep[i + 1..]
                    .iter_mut()
                    .zip(&self.pls[i + 1..])
                    .take_while(|(_, x)| x.depth > depth)
                    .for_each(|(k, _)| *k = true);
            }
        }
        if !keep.contains(&true) {
            return None;
        }
        let kept: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
        for i in kept {
            let mut depth = self.pls[i].depth;
            for j in (0..i).rev() {
                if self.pls[j].depth < depth {
                    (keep[j], depth) = (true, self.pls[j].depth);
                }
            }
        }
        let mut remap: Vec<Option<usize>> = vec![None; self.tracks.len()];
        let mut order = Vec::new();
        for &m in self
            .members
            .iter()
            .zip(&keep)
            .filter(|p| *p.1)
            .flat_map(|p| p.0)
        {
            if remap[m].is_none() {
                remap[m] = Some(order.len());
                order.push(m);
            }
        }
        let kept_of = |v: &[Playlist]| {
            v.iter()
                .zip(&keep)
                .filter(|p| *p.1)
                .map(|p| p.0.clone())
                .collect()
        };
        Some(Snapshot {
            tracks: order.iter().map(|&o| self.tracks[o].clone()).collect(),
            pls: kept_of(&self.pls),
            members: self
                .members
                .iter()
                .zip(&keep)
                .filter(|p| *p.1)
                .map(|(m, _)| m.iter().filter_map(|&i| remap[i]).collect())
                .collect(),
            colors: self
                .colors
                .iter()
                .filter_map(|(&i, &c)| remap[i].map(|n| (n, c)))
                .collect(),
            mytags: self.mytags.clone(),
            tagged: self
                .tagged
                .iter()
                .filter_map(|&(i, c, j)| remap[i].map(|n| (n, c, j)))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Library, Track};

    struct Lib(Vec<Track>);

    impl Library for Lib {
        fn kind(&self) -> &str {
            "test"
        }
        fn playlists(&self) -> Result<Vec<Playlist>, String> {
            let p = |id: &str, depth, is_folder| Playlist {
                id: id.into(),
                name: id.into(),
                depth,
                is_folder,
            };
            Ok(vec![
                p("f", 0, true),
                p("a", 1, false),
                p("b", 1, false),
                p("c", 0, false),
            ])
        }
        fn tracks(&self, playlist: Option<&str>) -> Result<Vec<Track>, String> {
            let pick = |ix: &[usize]| ix.iter().map(|&i| self.0[i].clone()).collect();
            Ok(match playlist {
                None => self.0.clone(),
                Some("a") => pick(&[2, 0]),
                Some("b") => pick(&[1]),
                _ => pick(&[3]),
            })
        }
    }

    fn lib() -> Lib {
        Lib((0..4)
            .map(|i| Track {
                title: format!("t{i}"),
                ..Default::default()
            })
            .collect())
    }

    #[test]
    fn narrowed_keeps_tracks_in_playlist_order_and_the_folders_above() {
        let full = Snapshot::take(&lib()).unwrap();
        assert!(full.narrowed(&["nope"]).is_none());
        let s = full.narrowed(&["f"]).unwrap();
        let shape = |s: &Snapshot| {
            s.playlists()
                .map(|(p, m)| (p.name.clone(), p.depth, m.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shape(&s),
            [
                ("f".to_string(), 0, 0),
                ("a".to_string(), 1, 2),
                ("b".to_string(), 1, 1)
            ]
        );
        let titles: Vec<_> = s.tracks().iter().map(|(t, _)| t.title.as_str()).collect();
        assert_eq!(titles, ["t2", "t0", "t1"]);
        let members: Vec<Vec<usize>> = s.playlists().map(|(_, m)| m.to_vec()).collect();
        assert_eq!(members, [vec![], vec![0, 1], vec![2]]);
        // A playlist inside a folder keeps its folder; with a sibling at the top level.
        let s = full.narrowed(&["b", "c"]).unwrap();
        assert_eq!(
            shape(&s),
            [
                ("f".to_string(), 0, 0),
                ("b".to_string(), 1, 1),
                ("c".to_string(), 0, 1)
            ]
        );
        let titles: Vec<_> = s.tracks().iter().map(|(t, _)| t.title.as_str()).collect();
        assert_eq!(titles, ["t1", "t3"]);
    }

    #[test]
    fn add_store_appends_user_playlists_colours_and_mytags() {
        let store = Store::in_memory().unwrap();
        let mut s = Snapshot::take(&lib()).unwrap();
        let k = |i: usize| store::key(&s.tracks()[i].0);
        store.set_color(&k(1), Some(3)).unwrap();
        let folder = store.add_node(None, "Mine", NodeKind::Folder).unwrap();
        let pl = store
            .add_node(Some(folder), "Set", NodeKind::Playlist)
            .unwrap();
        store
            .add_entries(pl, &[k(3), k(0), "gone|1".into()])
            .unwrap();
        let cat = store.add_mytag_category("Mood").unwrap();
        let tag = store.add_mytag(cat, "Dark").unwrap();
        store.set_track_mytag(&k(2), tag, true).unwrap();
        s.add_store(&store).unwrap();
        assert_eq!((s.color(1), s.color(0)), (3, 0));
        let pls: Vec<_> = s
            .playlists()
            .skip(4)
            .map(|(p, m)| (p.name.clone(), p.depth, p.is_folder, m.to_vec()))
            .collect();
        assert_eq!(
            pls,
            [
                ("Mine".to_string(), 0, true, vec![]),
                ("Set".to_string(), 1, false, vec![3, 0])
            ]
        );
        let (cats, tagged) = s.mytags();
        assert_eq!(
            (cats, tagged),
            (
                &[("Mood".to_string(), vec!["Dark".to_string()])][..],
                &[(2, 0, 0)][..]
            )
        );
    }
}
