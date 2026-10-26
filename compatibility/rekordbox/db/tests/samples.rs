#[path = "../examples/make_fixture.rs"]
mod fixture;

use openfader_plugin::library::{LibraryPlugin, Source};
use compat_rekordbox_common::anlz;
use compat_rekordbox_db::MasterDb;
use serde_json::{Value, json};
use std::{fs, path::Path};

fn is_history(id: &str) -> bool {
    id.starts_with("history:")
}

fn open(out: &Path, settings: Value) -> Result<Box<dyn Source>, String> {
    MasterDb::open(&out.join("master.db"), &settings)
}

fn have_samples() -> bool {
    let ok = fixture::spp_dir().join("Anlz").is_dir();
    if !ok {
        eprintln!("skip: .local/extract/spp/Anlz missing");
    }
    ok
}

#[test]
fn dats_match_xml() {
    if !have_samples() {
        return;
    }
    let spp = fixture::spp_dir();
    let tracks = fixture::xml_tracks(&spp).unwrap();
    let mut n = 0;
    for d in fs::read_dir(spp.join("Anlz")).unwrap() {
        let p = d.unwrap().path();
        let a = anlz::parse(&fs::read(&p).unwrap()).unwrap();
        let ppth = a.path.expect("PPTH");
        let name = ppth.rsplit(['/', '\\']).next().unwrap();
        let (_, _, bpm) = tracks
            .iter()
            .find(|(_, loc, _)| loc.rsplit('/').next() == Some(name))
            .unwrap_or_else(|| panic!("{} has no XML track for {ppth}", p.display()));
        assert!(!a.beats.is_empty(), "{}", p.display());
        assert!(
            a.beats.iter().all(|b| b.bpm_x100 as i64 == *bpm),
            "{} tempo",
            p.display()
        );
        assert!(
            a.beats.windows(2).all(|w| w[0].time_ms < w[1].time_ms),
            "{} times",
            p.display()
        );
        assert!(
            a.beats.iter().all(|b| (1..=4).contains(&b.bar_pos)),
            "{} bar_pos",
            p.display()
        );
        assert_eq!(a.preview.len(), 400, "{}", p.display());
        assert!(a.cues.is_empty(), "{}", p.display());
        n += 1;
    }
    assert_eq!(n, tracks.len());
}

#[test]
fn fixture_library() {
    if !have_samples() {
        return;
    }
    let Some(key) = fixture::key() else {
        eprintln!("skip: no db key");
        return;
    };
    let out = std::env::temp_dir().join(format!("openfader-fixture-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    fixture::build(&fixture::spp_dir(), &out, &key).unwrap();
    assert!(
        fixture::build(&fixture::spp_dir(), &out, &key).is_err(),
        "must refuse to overwrite"
    );

    let lib = open(&out, json!({"key": key})).unwrap();
    let all = lib.tracks(None).unwrap();
    assert!(all.iter().all(|t| t.title != "Deleted"));
    assert!(
        all.iter()
            .any(|t| t.title == "Unanalysed" && t.analysis_ref.is_none())
    );
    // Key/genre come from the djmdKey/djmdGenre joins; the unanalysed row has neither.
    for t in &all {
        let (key, genre) = if t.title == "Unanalysed" {
            ("", "")
        } else if t.title.starts_with("Breaks") {
            ("8A", "Breaks")
        } else {
            ("8A", "House")
        };
        assert_eq!(
            (t.key.as_str(), t.genre.as_str()),
            (key, genre),
            "{}",
            t.title
        );
        assert_eq!(
            t.album,
            if t.title == "Unanalysed" {
                ""
            } else {
                "Sample Pack Vol. 1"
            },
            "{}",
            t.title
        ); // djmdAlbum join
    }
    for t in all.iter().filter(|t| t.analysis_ref.is_some()) {
        assert!(
            std::path::Path::new(t.analysis_ref.as_ref().unwrap()).exists(),
            "{}",
            t.title
        );
    }

    let pls = lib.playlists().unwrap();
    let i = pls
        .iter()
        .position(|p| p.name == "Packs")
        .expect("Packs folder");
    assert!(pls[i].is_folder && pls[i].depth == 0);
    assert!(pls[i + 1].name == "Breaks" && pls[i + 1].depth == 1 && !pls[i + 1].is_folder);
    assert!(
        all.iter()
            .all(|t| t.file.as_ref().is_some_and(|f| f.is_absolute()))
    );
    let breaks = pls
        .iter()
        .find(|p| p.name == "Breaks")
        .expect("Breaks playlist");
    let titles: Vec<_> = lib
        .tracks(Some(&breaks.id))
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, ["Breaks3", "Breaks2", "Breaks1"]);

    // Histories: last top-level folder, nested like playlists, entries by TrackNo, deleted rows hidden.
    let h = pls
        .iter()
        .position(|p| p.name == "Histories")
        .expect("Histories folder");
    assert_eq!(pls[h].id, "history:");
    let shape: Vec<_> = pls[h..]
        .iter()
        .map(|p| (p.name.as_str(), p.depth, p.is_folder))
        .collect();
    assert_eq!(
        shape,
        [
            ("Histories", 0, true),
            ("2026", 1, true),
            ("Friday", 2, false),
            ("HISTORY 002", 1, false)
        ]
    );
    assert!(
        pls[..h].iter().all(|p| !is_history(&p.id)) && pls[h..].iter().all(|p| is_history(&p.id))
    );
    let list = |id: &str| {
        lib.tracks(Some(id))
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect::<Vec<_>>()
    };
    let house = list("3");
    assert_eq!(
        list("history:h2"),
        house.iter().rev().cloned().collect::<Vec<_>>()
    );
    assert_eq!(list("history:h3"), [house[0].clone()]); // the deleted song-history row is hidden
    assert!(list("history:").is_empty() && list("history:h1").is_empty()); // folders list no tracks

    // Smart lists: listed like plain playlists, resolved from their conditions, ordered by title.
    let titles = |name: &str| {
        let pl = pls
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} listed"));
        assert!(!pl.is_folder && pl.depth == 0);
        lib.tracks(Some(&pl.id))
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect::<Vec<_>>()
    };
    // 124.00..=140.00 BPM; the unanalysed row (128) matches too, the deleted one never.
    assert_eq!(
        titles("Mid tempo"),
        [
            "Breaks1",
            "Breaks2",
            "House2",
            "House3",
            "Techno1",
            "Unanalysed"
        ]
    );
    // ALL: title contains "house" (case-insensitive) AND in the BPM range.
    assert_eq!(titles("House 124-140"), ["House2", "House3"]);
    // ANY: title is Techno1 OR BPM > 145.
    assert_eq!(titles("Techno or fast"), ["House4", "Techno1"]);

    // An unsupported condition is an error that names it, and the list stays visible.
    let rw = rusqlite::Connection::open(out.join("master.db")).unwrap();
    rw.pragma_update(None, "key", &key).unwrap();
    rw.execute(
        "insert into djmdPlaylist(ID, Seq, Name, Attribute, ParentID, SmartList) values ('9', 9, 'Bad', 4, 'root', ?1)",
        ["<NODE Id=\"1\" LogicalOperator=\"1\" AutomaticUpdate=\"0\"><CONDITION PropertyName=\"mixName\" Operator=\"1\" ValueUnit=\"\" ValueLeft=\"x\" ValueRight=\"\"/></NODE>"],
    )
    .unwrap();
    drop(rw);
    // The library opens the file immutable, so a later write needs a fresh handle.
    let err = open(&out, json!({"key": key}))
        .unwrap()
        .tracks(Some("9"))
        .unwrap_err();
    assert!(err.contains("mixName"), "{err}");

    // OPENFADER_PATH_MAP: a map from the pack's folder rewrites every `FolderPath` under it.
    let house2 = all
        .iter()
        .find(|t| t.title == "House2")
        .unwrap()
        .file
        .clone()
        .unwrap();
    let map = format!("{}=>/mapped/music", house2.parent().unwrap().display());
    let remapped = open(&out, json!({"key": key, "path_map": map})).unwrap();
    let t = remapped
        .tracks(None)
        .unwrap()
        .into_iter()
        .find(|t| t.title == "House2")
        .unwrap();
    assert_eq!(
        t.file.unwrap(),
        Path::new("/mapped/music/").join(house2.file_name().unwrap())
    );

    assert!(open(&out, json!({"key": "wrong-key"})).is_err());
    let _ = fs::remove_dir_all(Path::new(&out));
}
