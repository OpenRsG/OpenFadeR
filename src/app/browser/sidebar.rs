//! The playlist tree beside the track table: what every sidebar row is ([`Entry`]) and the label it shows.
//!
//! Top to bottom: All tracks, Tag List, Session history, Duplicate tracks (the first three rows keep their
//! numbers, `BrowserState.tag-row` is row 1), then **My Playlists** (our folders / playlists / smart playlists from
//! the store), **History** (one entry per day the store logged), **Tags** (categories, their tags) and **Library**
//! (the opened source's own playlists, read-only; its "Histories" folder is among them). **Library health** (heading and one row per
//! check, [`super::health::rows`]) is added before Library when the tree is rebuilt.

use openfader::{
    library::Playlist,
    plugin::device::MAX_BANKS,
    store::{HotCueBank, Tag, TagGroup, Node, NodeKind},
};
use std::collections::HashMap;

/// Sidebar row of the session Tag List (row 0 is "All tracks").
pub const TAG_ROW: usize = 1;
/// Sidebar row of the Session history, right after the Tag List.
pub const HISTORY_ROW: usize = 2;

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    All,
    Tags,
    Session,
    Dupes,
    UserRoot,
    /// A node of My Playlists (store id).
    User(i64),
    /// "Cue Bank" heading above the lists (Preferences > View > Layout > Media Browser).
    BankRoot,
    /// A Cue Bank of the store (id).
    Bank(i64),
    /// "Stages" heading above INBOX → REVIEW → COLLECTION (the store's [`openfader::store::Stage`]s).
    StageRoot,
    /// A stage by id.
    Stage(i64),
    HistoryRoot,
    /// A day of the stored play history, `YYYY-MM-DD`.
    Day(String),
    TagsRoot,
    Category(i64),
    Tag(i64),
    LibRoot,
    /// Index into the library's `playlists()`.
    Lib(usize),
    /// "All Tracks" first under My Playlists (Preferences > View > Layout > Display All Tracks in the Playlists).
    PlaylistsAll,
    /// The CUE Analysis playlist (Preferences > View > Layout > Browser panel): tracks CUE Analysis set cues on.
    CueAnalysis,
    /// "Library health" heading (above its checks, before Library): see [`super::health`].
    HealthRoot,
    /// One check of Library health.
    Health(crate::app::library::health::Check),
    /// "Devices" heading above the mounted devices.
    DevicesRoot,
    /// An external source (a device, iTunes, library xml, Explorer) by index into the browser's sources; it opens when chosen.
    Source(usize),
    /// "All tracks" of an opened source.
    SourceAll(usize),
    /// A playlist of an opened source: (source, index into its playlists).
    SourceList(usize, usize),
}

/// One external source as the tree shows it.
pub struct SourceRows {
    pub device: bool,
    pub name: String,
    /// Opened: its "All tracks" and playlists are listed under it.
    pub open: bool,
    pub playlists: Vec<Playlist>,
}

/// One stage as the tree shows it, with the tracks waiting in it.
pub struct StageRow {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

pub struct Inputs<'a> {
    pub tag_list: usize,
    pub session: usize,
    pub lib: &'a [Playlist],
    pub nodes: &'a [Node],
    /// Days with plays, newest first, and how many tracks each has.
    pub days: &'a [(String, usize)],
    pub cats: &'a [TagGroup],
    pub tags: &'a [Tag],
    pub sources: &'a [SourceRows],
    pub all_tracks: bool,
    /// Size of the CUE Analysis playlist when it is shown.
    pub cue_analysis: Option<usize>,
    /// The store's Cue Banks when the Media Browser shows them.
    pub banks: Option<&'a [HotCueBank]>,
    /// INBOX → REVIEW → COLLECTION, first to last (none: no heading).
    pub stages: &'a [StageRow],
}

fn indent(depth: usize, label: &str) -> String {
    format!("{}{label}", "    ".repeat(depth))
}

/// Label of a library playlist `base` levels in, plus its own depth; folders get the `▸` marker.
fn playlist_label(base: usize, p: &Playlist) -> String {
    indent(
        base + p.depth,
        &format!("{}{}", if p.is_folder { "▸ " } else { "" }, p.name),
    )
}

/// Every row's entry and label, in display order.
pub fn build(i: &Inputs) -> (Vec<Entry>, Vec<String>) {
    let mut rows = vec![
        (Entry::All, tr!("All tracks").to_owned()),
        (Entry::Tags, tr!("Tag List ({})", i.tag_list)),
        (Entry::Session, tr!("Session history ({})", i.session)),
        (Entry::Dupes, tr!("Duplicate tracks").to_owned()),
        (Entry::UserRoot, tr!("▸ My Playlists").to_owned()),
    ];
    if let Some(n) = i.cue_analysis {
        rows.insert(4, (Entry::CueAnalysis, tr!("CUE Analysis ({})", n)));
    }
    if i.all_tracks {
        rows.push((Entry::PlaylistsAll, indent(1, tr!("All Tracks"))));
    }
    let mut depth: HashMap<i64, usize> = HashMap::new();
    for n in i.nodes {
        // Nodes come depth-first, so the parent is already known; an unknown parent counts as top level.
        let d = n.parent.and_then(|p| depth.get(&p)).map_or(1, |d| d + 1);
        depth.insert(n.id, d);
        let label = match n.kind {
            NodeKind::Folder => format!("▸ {}", n.name),
            NodeKind::Playlist => n.name.clone(),
            NodeKind::Smart => tr!("{} (smart)", n.name),
        };
        rows.push((Entry::User(n.id), indent(d, &label)));
    }
    if let Some(banks) = i.banks {
        rows.push((Entry::BankRoot, tr!("▸ Cue Bank").to_owned()));
        rows.extend(banks.iter().map(|b| {
            let label = format!("{} ({}/{MAX_BANKS})", b.name, b.slots.len());
            (Entry::Bank(b.id), indent(1, &label))
        }));
    }
    if !i.stages.is_empty() {
        rows.push((Entry::StageRoot, tr!("▸ Stages").to_owned()));
        rows.extend(i.stages.iter().map(|s| {
            (
                Entry::Stage(s.id),
                indent(1, &format!("{} ({})", s.name, s.count)),
            )
        }));
    }
    rows.push((Entry::HistoryRoot, tr!("▸ History").to_owned()));
    rows.extend(
        i.days
            .iter()
            .map(|(day, n)| (Entry::Day(day.clone()), indent(1, &format!("{day} ({n})")))),
    );
    rows.push((Entry::TagsRoot, tr!("▸ Tags").to_owned()));
    for c in i.cats {
        rows.push((Entry::Category(c.id), indent(1, &format!("▸ {}", c.name))));
        rows.extend(
            i.tags
                .iter()
                .filter(|t| t.category == c.id)
                .map(|t| (Entry::Tag(t.id), indent(2, &t.name))),
        );
    }
    rows.push((Entry::LibRoot, tr!("▸ Library (read-only)").to_owned()));
    rows.extend(
        i.lib
            .iter()
            .enumerate()
            .map(|(n, p)| (Entry::Lib(n), playlist_label(1, p))),
    );
    if i.sources.iter().any(|s| s.device) {
        rows.push((Entry::DevicesRoot, tr!("▸ Devices").to_owned()));
    }
    for (n, s) in i.sources.iter().enumerate() {
        let d = usize::from(s.device);
        rows.push((
            Entry::Source(n),
            indent(
                d,
                &if s.device {
                    s.name.clone()
                } else {
                    format!("▸ {}", s.name)
                },
            ),
        ));
        if s.open {
            rows.push((Entry::SourceAll(n), indent(d + 1, tr!("All tracks"))));
            rows.extend(
                s.playlists
                    .iter()
                    .enumerate()
                    .map(|(j, p)| (Entry::SourceList(n, j), playlist_label(d + 1, p))),
            );
        }
    }
    rows.into_iter().unzip()
}

/// The library playlist `at` with everything nested under it, as (index, depth below `at`); `at` itself first.
/// Empty when `at` is out of range.
pub fn subtree(lib: &[Playlist], at: usize) -> Vec<(usize, usize)> {
    let Some(root) = lib.get(at) else {
        return Vec::new();
    };
    let nested = lib[at + 1..]
        .iter()
        .take_while(|p| p.depth > root.depth)
        .enumerate();
    std::iter::once((at, 0))
        .chain(nested.map(|(n, p)| (at + 1 + n, p.depth - root.depth)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(name: &str, depth: usize, is_folder: bool) -> Playlist {
        Playlist {
            id: name.into(),
            name: name.into(),
            depth,
            is_folder,
        }
    }

    fn node(id: i64, parent: Option<i64>, name: &str, kind: NodeKind) -> Node {
        Node {
            id,
            parent,
            name: name.into(),
            kind,
            pos: 0,
        }
    }

    #[test]
    fn rows_come_in_the_documented_order_with_indentation() {
        let lib = [pl("Folder", 0, true), pl("Inner", 1, false)];
        let nodes = [
            node(1, None, "Set", NodeKind::Folder),
            node(2, Some(1), "Warmup", NodeKind::Playlist),
            node(3, None, "House", NodeKind::Smart),
        ];
        let days = [("2026-10-13".to_owned(), 3), ("2026-10-12".to_owned(), 1)];
        let cats = [
            TagGroup {
                id: 5,
                name: "Genre".into(),
                pos: 0,
            },
            TagGroup {
                id: 6,
                name: "Mood".into(),
                pos: 1,
            },
        ];
        let tags = [
            Tag {
                id: 9,
                category: 5,
                name: "Deep".into(),
                pos: 0,
            },
            Tag {
                id: 10,
                category: 6,
                name: "Dark".into(),
                pos: 0,
            },
        ];
        let banks = [HotCueBank {
            id: 7,
            name: "Peak".into(),
            slots: Vec::new(),
        }];
        let stages = [
            StageRow {
                id: 1,
                name: "INBOX".into(),
                count: 2,
            },
            StageRow {
                id: 3,
                name: "COLLECTION".into(),
                count: 5,
            },
        ];
        let (entries, labels) = build(&Inputs {
            tag_list: 2,
            session: 4,
            lib: &lib,
            nodes: &nodes,
            days: &days,
            cats: &cats,
            tags: &tags,
            sources: &[],
            all_tracks: false,
            cue_analysis: None,
            banks: Some(&banks),
            stages: &stages,
        });
        assert_eq!(entries[TAG_ROW], Entry::Tags);
        assert_eq!(entries[HISTORY_ROW], Entry::Session);
        assert_eq!(
            labels,
            [
                "All tracks",
                "Tag List (2)",
                "Session history (4)",
                "Duplicate tracks",
                "▸ My Playlists",
                "    ▸ Set",
                "        Warmup",
                "    House (smart)",
                "▸ Cue Bank",
                "    Peak (0/8)",
                "▸ Stages",
                "    INBOX (2)",
                "    COLLECTION (5)",
                "▸ History",
                "    2026-10-13 (3)",
                "    2026-10-12 (1)",
                "▸ Tags",
                "    ▸ Genre",
                "        Deep",
                "    ▸ Mood",
                "        Dark",
                "▸ Library (read-only)",
                "    ▸ Folder",
                "        Inner",
            ]
        );
        assert_eq!(entries.len(), labels.len());
        assert_eq!(entries[6], Entry::User(2));
        assert_eq!(entries[9], Entry::Bank(7));
        assert_eq!(entries[10], Entry::StageRoot);
        assert_eq!(entries[12], Entry::Stage(3));
        assert_eq!(entries[14], Entry::Day("2026-10-13".into()));
        assert_eq!(entries[18], Entry::Tag(9));
        assert_eq!(entries[23], Entry::Lib(1));
    }

    #[test]
    fn an_empty_store_still_shows_every_section() {
        let (e, _) = build(&Inputs {
            tag_list: 0,
            session: 0,
            lib: &[],
            nodes: &[],
            days: &[],
            cats: &[],
            tags: &[],
            sources: &[],
            all_tracks: false,
            cue_analysis: None,
            banks: None,
            stages: &[],
        });
        assert_eq!(
            e,
            [
                Entry::All,
                Entry::Tags,
                Entry::Session,
                Entry::Dupes,
                Entry::UserRoot,
                Entry::HistoryRoot,
                Entry::TagsRoot,
                Entry::LibRoot
            ]
        );
    }

    #[test]
    fn subtree_takes_the_folder_and_all_it_nests() {
        let lib = [
            pl("A", 0, true),
            pl("a1", 1, false),
            pl("a2", 1, true),
            pl("a2x", 2, false),
            pl("B", 0, false),
        ];
        assert_eq!(subtree(&lib, 0), [(0, 0), (1, 1), (2, 1), (3, 2)]);
        assert_eq!(subtree(&lib, 2), [(2, 0), (3, 1)]);
        assert_eq!(subtree(&lib, 4), [(4, 0)]);
        assert!(subtree(&lib, 9).is_empty());
    }

    #[test]
    fn devices_and_other_sources_open_into_their_playlists() {
        let src = |device, name: &str, open| SourceRows {
            device,
            name: name.into(),
            open,
            playlists: vec![pl("Set", 0, true), pl("Inner", 1, false)],
        };
        let sources = [
            src(true, "USB (vfat)", true),
            src(true, "SD", false),
            src(false, "iTunes", true),
        ];
        let (e, l) = build(&Inputs {
            tag_list: 0,
            session: 0,
            lib: &[],
            nodes: &[],
            days: &[],
            cats: &[],
            tags: &[],
            sources: &sources,
            all_tracks: true,
            cue_analysis: Some(3),
            banks: None,
            stages: &[],
        });
        assert_eq!(l[4], "CUE Analysis (3)");
        assert_eq!(l[6], "    All Tracks");
        let at = l.iter().position(|x| x == "▸ Devices").unwrap();
        assert_eq!(
            &l[at..],
            [
                "▸ Devices",
                "    USB (vfat)",
                "        All tracks",
                "        ▸ Set",
                "            Inner",
                "    SD",
                "▸ iTunes",
                "    All tracks",
                "    ▸ Set",
                "        Inner"
            ]
        );
        assert_eq!(
            (&e[at + 1], &e[at + 2], &e[at + 4], &e[at + 5]),
            (
                &Entry::Source(0),
                &Entry::SourceAll(0),
                &Entry::SourceList(0, 1),
                &Entry::Source(1)
            )
        );
    }
}
