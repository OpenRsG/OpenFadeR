//! Duplicated File Manager (File > Display All Duplicated Files) and the finder behind it and behind the
//! browser's "Duplicate tracks" list. Two tracks are duplicates when title and artist match (case and extra spaces
//! ignored) and their lengths are within [`LENGTH_SLACK`] seconds; tracks without a title are never grouped.
//!
//! The manager lists the duplicate sets with one representative file each (default: highest bitrate, then longest).
//! MERGE adds the cues of the others to the representative (a memory cue within 50 ms of one it has, or a hot cue on a slot
//! it has, is not added), moves their playlist places, tags, plays and edits onto it, and removes the others from the
//! Collection; DELETE removes the ticked tracks from the Collection. Audio files are never deleted.

use super::Ctx;
use crate::app::{DupeRow, LibraryState, analyze};
use openfader::{analysis::Cue, library::Track, store};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{collections::HashMap, rc::Rc};

/// Longest length difference (s) between duplicates, measured from the shortest track of a group.
const LENGTH_SLACK: i64 = 1;

/// Whitespace runs collapsed and case folded: the key titles and artists are compared by.
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out.to_lowercase()
}

/// Groups of indices into `tracks` (each ≥ 2, ascending), ordered by their first member. A group's lengths all lie
/// within [`LENGTH_SLACK`] of its shortest track.
pub fn find(tracks: &[Track]) -> Vec<Vec<usize>> {
    let mut by_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, t) in tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.title.trim().is_empty())
    {
        by_name
            .entry((norm(&t.title), norm(&t.artist)))
            .or_default()
            .push(i);
    }
    let mut groups = Vec::new();
    for mut same in by_name.into_values().filter(|v| v.len() > 1) {
        same.sort_by_key(|&i| (tracks[i].length_s, i));
        let mut run: Vec<usize> = Vec::new();
        for i in same {
            if run
                .first()
                .is_some_and(|&h| tracks[i].length_s - tracks[h].length_s > LENGTH_SLACK)
            {
                groups.extend(Some(std::mem::take(&mut run)).filter(|r| r.len() > 1));
            }
            run.push(i);
        }
        groups.extend(Some(run).filter(|r| r.len() > 1));
    }
    for g in &mut groups {
        g.sort_unstable();
    }
    groups.sort_unstable_by_key(|g| g[0]);
    groups
}

/// `rep` plus those cues of `others` it does not already have.
pub fn merge_cues(mut rep: Vec<Cue>, others: &[Vec<Cue>]) -> Vec<Cue> {
    for c in others.iter().flatten() {
        let taken = rep.iter().any(|r| {
            if c.hot_cue == 0 {
                r.hot_cue == 0 && r.time_ms.abs_diff(c.time_ms) < 50
            } else {
                r.hot_cue == c.hot_cue
            }
        });
        if !taken {
            rep.push(c.clone());
        }
    }
    rep
}

/// The representative of a set: highest bitrate, then longest, then first.
fn representative(set: &[Track]) -> usize {
    (0..set.len())
        .max_by_key(|&i| (set[i].bitrate, set[i].length_s, std::cmp::Reverse(i)))
        .unwrap_or(0)
}

/// The manager's data: duplicate sets, the representative of each (index into its set) and the ticked tracks.
#[derive(Default)]
pub struct State {
    sets: Vec<Vec<Track>>,
    keep: Vec<usize>,
    ticked: Vec<Vec<bool>>,
}

impl State {
    fn rows(&self) -> Vec<DupeRow> {
        let mut out = Vec::new();
        for (g, set) in self.sets.iter().enumerate() {
            for (k, t) in set.iter().enumerate() {
                let file = t
                    .file
                    .as_ref()
                    .map_or_else(String::new, |f| f.display().to_string());
                let kbps = if t.bitrate > 0 {
                    format!(" · {} kbps", t.bitrate)
                } else {
                    String::new()
                };
                out.push(DupeRow {
                    group: g as i32,
                    title: format!("{} — {}", t.title, t.artist).into(),
                    info: format!("{}{kbps} · {file}", t.length_text()).into(),
                    keep: self.keep[g] == k,
                    selected: self.ticked[g][k],
                });
            }
        }
        out
    }

    /// Row `i` of the flat list as (set, member).
    fn at(&self, mut i: usize) -> Option<(usize, usize)> {
        for (g, set) in self.sets.iter().enumerate() {
            if i < set.len() {
                return Some((g, i));
            }
            i -= set.len();
        }
        None
    }
}

fn show(c: &Rc<Ctx>) {
    let app = c.app();
    let s = app.global::<LibraryState>();
    let st = c.state.borrow();
    s.set_dupes_rows(ModelRc::new(VecModel::from(st.dupes.rows())));
}

/// Opens the manager with a fresh scan.
pub fn open(c: &Rc<Ctx>) {
    c.app().global::<LibraryState>().set_dupes_open(true);
    scan(c);
}

pub fn scan(c: &Rc<Ctx>) {
    let tracks = match c.lib.tracks(None) {
        Ok(t) => t,
        Err(e) => return c.say(&tr!("duplicates: {}", e)),
    };
    let sets: Vec<Vec<Track>> = find(&tracks)
        .into_iter()
        .map(|g| g.into_iter().map(|i| tracks[i].clone()).collect())
        .collect();
    let n: usize = sets.iter().map(Vec::len).sum();
    let status = if sets.is_empty() {
        tr!("No duplicate track found").to_owned()
    } else {
        tr!(
            "Duplicated tracks are found: {} tracks in {} sets",
            n,
            sets.len()
        )
    };
    c.state.borrow_mut().dupes = State {
        keep: sets.iter().map(|s| representative(s)).collect(),
        ticked: sets.iter().map(|s| vec![false; s.len()]).collect(),
        sets,
    };
    c.app()
        .global::<LibraryState>()
        .set_dupes_status(status.into());
    show(c);
}

pub fn toggle(c: &Rc<Ctx>, i: usize) {
    {
        let mut st = c.state.borrow_mut();
        let Some((g, k)) = st.dupes.at(i) else { return };
        st.dupes.ticked[g][k] ^= true;
    }
    show(c);
}

pub fn keep(c: &Rc<Ctx>, i: usize) {
    {
        let mut st = c.state.borrow_mut();
        let Some((g, k)) = st.dupes.at(i) else { return };
        st.dupes.keep[g] = k;
        st.dupes.ticked[g][k] = false;
    }
    show(c);
}

/// MERGE: every set into its representative.
pub fn merge(c: &Rc<Ctx>) {
    if c.protected(tr!("merge duplicates")) {
        return;
    }
    let st = std::mem::take(&mut c.state.borrow_mut().dupes);
    let (mut removed, mut merged_cues) = (Vec::new(), 0);
    for (g, set) in st.sets.iter().enumerate() {
        let rep = &set[st.keep[g]];
        let rep_key = store::key(rep);
        let others: Vec<&Track> = set
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != st.keep[g])
            .map(|(_, t)| t)
            .collect();
        let mine = analyze::library_cues(&*c.lib, rep);
        let theirs: Vec<Vec<Cue>> = others
            .iter()
            .map(|t| analyze::library_cues(&*c.lib, t))
            .collect();
        let before = mine.len();
        let merged = merge_cues(mine, &theirs);
        if merged.len() != before {
            merged_cues += merged.len() - before;
            if let Err(e) = c.store.set_cues(&rep_key, Some(&merged)) {
                return c.say(&tr!("merge failed: {}", e));
            }
        }
        for o in others {
            if let Err(e) = c.store.rekey(&store::key(o), &rep_key) {
                return c.say(&tr!("merge failed: {}", e));
            }
            removed.push(o.clone());
        }
    }
    c.remove_tracks(&removed);
    c.say(&tr!(
        "merged {} duplicate files · {} cues added to the representatives",
        removed.len(),
        merged_cues
    ));
    scan(c);
}

/// DELETE: the ticked tracks leave the Collection.
pub fn delete(c: &Rc<Ctx>) {
    if c.protected(tr!("remove tracks")) {
        return;
    }
    let ticked: Vec<Track> = {
        let st = c.state.borrow();
        st.dupes
            .sets
            .iter()
            .zip(&st.dupes.ticked)
            .flat_map(|(s, t)| {
                s.iter()
                    .zip(t)
                    .filter(|(_, on)| **on)
                    .map(|(t, _)| t.clone())
            })
            .collect()
    };
    if ticked.is_empty() {
        return c.say(tr!("tick the tracks to remove from the Collection"));
    }
    c.remove_tracks(&ticked);
    c.say(&tr!("removed {} tracks from the Collection", ticked.len()));
    scan(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(title: &str, artist: &str, length_s: i64) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            length_s,
            ..Track::default()
        }
    }

    #[test]
    fn same_title_artist_and_length_within_a_second_group() {
        let tracks = [
            t("Song", "Band", 200),
            t("Other", "Band", 200),
            t("song ", " BAND", 201),
            t("Song", "Band", 203),
            t("Song", "Band", 200),
        ];
        assert_eq!(find(&tracks), vec![vec![0, 2, 4]]);
    }

    #[test]
    fn length_is_measured_from_the_shortest_so_chains_do_not_drift() {
        assert_eq!(
            find(&[
                t("A", "", 100),
                t("A", "", 101),
                t("A", "", 102),
                t("A", "", 102)
            ]),
            vec![vec![0, 1], vec![2, 3]]
        );
    }

    #[test]
    fn different_artist_or_no_title_never_group() {
        assert!(
            find(&[
                t("A", "x", 100),
                t("A", "y", 100),
                t("", "x", 100),
                t("  ", "x", 100)
            ])
            .is_empty()
        );
        assert!(find(&[]).is_empty());
    }

    #[test]
    fn groups_come_out_in_library_order() {
        let tracks = [
            t("B", "", 10),
            t("A", "", 10),
            t("B", "", 10),
            t("A", "", 10),
        ];
        assert_eq!(find(&tracks), vec![vec![0, 2], vec![1, 3]]);
    }

    fn cue(hot_cue: u16, time_ms: u32) -> Cue {
        Cue {
            hot_cue,
            time_ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    #[test]
    fn merging_keeps_the_representatives_cues_and_adds_only_new_ones() {
        let rep = vec![cue(0, 1000), cue(1, 5000)];
        let other = vec![cue(0, 1030), cue(0, 9000), cue(1, 7000), cue(2, 7000)];
        let got = merge_cues(rep, &[other]);
        let key = |c: &Cue| (c.hot_cue, c.time_ms);
        assert_eq!(
            got.iter().map(key).collect::<Vec<_>>(),
            [(0, 1000), (1, 5000), (0, 9000), (2, 7000)]
        );
    }

    #[test]
    fn the_best_bitrate_then_the_longest_represents() {
        let mut set = vec![t("A", "", 100), t("A", "", 101), t("A", "", 100)];
        set[2].bitrate = 320;
        assert_eq!(representative(&set), 2);
        set[0].bitrate = 320;
        set[1].bitrate = 320;
        assert_eq!(representative(&set), 1); // equal bitrate: longest
    }
}
