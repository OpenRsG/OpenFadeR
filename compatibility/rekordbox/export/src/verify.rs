//! Reading what an export wrote back before it counts: the staged `export.pdb`, `exportExt.pdb` and `exportLibrary.db` are decoded
//! again and compared with what was encoded (tracks, playlists and their entries, histories, MyTag), both libraries must list the
//! same tracks, and the analysis files of the tracks written must give back as many cues as were encoded. Any difference is an
//! error worded as cause, consequence and action; the caller drops its [`Commit`], so the old files stay in use.

use super::open_keyed_ro;
use super::pdb::{
    db::{Db, Item, Node, Tags},
    read,
    rows::clip,
};
use super::run::CueCheck;
use openfader_plugin::library::commit::Commit;
use std::collections::HashMap;
use std::path::Path;

const DL: &str = "Device Library";
const OL: &str = "OneLibrary";

/// What a player would do and what to do, the same for every failed check.
const NEXT: &str = "A player would show a wrong or incomplete stick. Nothing on the stick was changed; try again, and use another stick if it happens again.";

fn unreadable(lib: &str, why: &str) -> String {
    format!("The {lib} just written cannot be read back ({why}). {NEXT}")
}

/// What read back different.
enum Diff<'a> {
    Track(&'a str),
    Playlist(&'a str),
    /// The histories, the tags, the list of tracks.
    Content,
}

/// One whole sentence per kind of difference, so each has its own translation.
fn differs(lib: &str, what: Diff) -> String {
    match what {
        Diff::Track(t) => format!(
            "The {lib} just written does not match what was exported (track {t} differs). {NEXT}"
        ),
        Diff::Playlist(p) => format!(
            "The {lib} just written does not match what was exported (playlist {p} differs). {NEXT}"
        ),
        Diff::Content => {
            format!(
                "The {lib} just written does not match what was exported (its content differs). {NEXT}"
            )
        }
    }
}

fn disagree(device_library: usize, one_library: usize) -> String {
    format!(
        "The Device Library and the OneLibrary just written list different tracks ({device_library} and {one_library}). {NEXT}"
    )
}

fn counts(lib: &str, what: &str, got: usize, want: usize) -> String {
    // One whole sentence per thing counted, so each one has its own translation.
    match what {
        "tracks" => format!("The {lib} just written lists {got} tracks instead of {want}. {NEXT}"),
        "playlists" => format!(
            "The {lib} just written holds {got} playlists and folders instead of {want}. {NEXT}"
        ),
        _ => {
            format!("The {lib} just written lists {got} playlist entries instead of {want}. {NEXT}")
        }
    }
}

/// Reads the staged files of `c` back and compares them with `db` and `tags` (what was encoded); `key` opens the OneLibrary.
pub(crate) fn staged(c: &Commit, db: &Db, tags: &Tags, key: &str) -> Result<(), String> {
    let (mut in_pdb, mut in_one) = (None, None);
    for (target, staged) in c.staged() {
        match target.file_name().and_then(|n| n.to_str()) {
            Some("export.pdb") => in_pdb = Some(pdb(staged, db)?),
            Some("exportExt.pdb") => ext(staged, tags)?,
            Some("exportLibrary.db") => in_one = Some(one(staged, key, db)?),
            _ => {}
        }
    }
    if let (Some(a), Some(b)) = (in_pdb, in_one) {
        if a != b {
            return Err(disagree(a.len(), b.len()));
        }
    }
    Ok(())
}

/// The ids of the tracks `export.pdb` at `staged` lists, sorted, once it matches `db`.
fn pdb(staged: &Path, db: &Db) -> Result<Vec<u32>, String> {
    let bytes = std::fs::read(staged).map_err(|e| unreadable(DL, &e.to_string()))?;
    let back = read::parse(&bytes).map_err(|e| unreadable(DL, &e))?;
    if back.tracks.len() != db.tracks.len() {
        return Err(counts(DL, "tracks", back.tracks.len(), db.tracks.len()));
    }
    // The writer keeps the first 255 characters of a text (`rows::clip`): that is what must read back.
    for (got, want) in back.tracks.iter().zip(&db.tracks) {
        let c = |s: &str| clip(s).to_owned();
        let want = Item {
            title: c(&want.title),
            artist: c(&want.artist),
            album: c(&want.album),
            genre: c(&want.genre),
            key: c(&want.key),
            artwork: want.artwork.as_deref().map(c),
            comment: c(&want.comment),
            date_added: c(&want.date_added),
            analyze_date: c(if want.analyze_date.is_empty() {
                &db.analyze_date
            } else {
                &want.analyze_date
            }),
            analyze_path: c(&want.analyze_path),
            filename: c(&want.filename),
            file_path: c(&want.file_path),
            ..want.clone()
        };
        if format!("{got:?}") != format!("{want:?}") {
            return Err(differs(DL, Diff::Track(&want.title)));
        }
    }
    if back.nodes.len() != db.nodes.len() {
        return Err(counts(DL, "playlists", back.nodes.len(), db.nodes.len()));
    }
    let have: HashMap<u32, &Node> = back.nodes.iter().map(|n| (n.id, n)).collect();
    for n in &db.nodes {
        match have.get(&n.id) {
            Some(b)
                if (b.parent, &b.name[..], b.is_folder)
                    == (n.parent, clip(&n.name), n.is_folder) =>
            {
                if b.tracks != n.tracks {
                    let (got, want) = (
                        back.nodes.iter().map(|n| n.tracks.len()).sum(),
                        db.nodes.iter().map(|n| n.tracks.len()).sum(),
                    );
                    return Err(if got != want {
                        counts(DL, "entries", got, want)
                    } else {
                        differs(DL, Diff::Playlist(&n.name))
                    });
                }
            }
            _ => return Err(differs(DL, Diff::Playlist(&n.name))),
        }
    }
    let seen: HashMap<u32, (&str, &[u32])> = back
        .histories
        .iter()
        .map(|h| (h.id, (h.name.as_str(), h.tracks.as_slice())))
        .collect();
    let same = back.histories.len() == db.histories.len()
        && db
            .histories
            .iter()
            .all(|h| seen.get(&h.id) == Some(&(h.name.as_str(), h.tracks.as_slice())));
    if !same {
        return Err(differs(DL, Diff::Content));
    }
    let mut ids: Vec<u32> = back.tracks.iter().map(|t| t.id).collect();
    ids.sort_unstable();
    Ok(ids)
}

/// [`pdb`] for one staged `export.pdb` alone (an edit that leaves the OneLibrary as it is).
pub(crate) fn pdb_file(staged: &Path, db: &Db) -> Result<(), String> {
    pdb(staged, db).map(drop)
}

fn ext(staged: &Path, tags: &Tags) -> Result<(), String> {
    let bytes = std::fs::read(staged).map_err(|e| unreadable("MyTag list", &e.to_string()))?;
    let back = read::parse_tags(&bytes).map_err(|e| unreadable("MyTag list", &e))?;
    if back.categories != tags.categories || back.assigned != tags.assigned {
        return Err(differs("MyTag list", Diff::Content));
    }
    Ok(())
}

/// The ids of the tracks `exportLibrary.db` at `staged` lists, sorted, once it matches `db`.
fn one(staged: &Path, key: &str, db: &Db) -> Result<Vec<u32>, String> {
    let conn = open_keyed_ro(staged, key).map_err(|e| unreadable(OL, &e))?;
    let n = |sql: &str| -> Result<usize, String> {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0))
            .map(|v| v as usize)
            .map_err(|e| unreadable(OL, &e.to_string()))
    };
    let tracks = n("select count(*) from content")?;
    if tracks != db.tracks.len() {
        return Err(counts(OL, "tracks", tracks, db.tracks.len()));
    }
    let lists = n("select count(*) from playlist")?;
    if lists != db.nodes.len() {
        return Err(counts(OL, "playlists", lists, db.nodes.len()));
    }
    let entries = n("select count(*) from playlist_content")?;
    let want: usize = db.nodes.iter().map(|n| n.tracks.len()).sum();
    if entries != want {
        return Err(counts(OL, "entries", entries, want));
    }
    let ids = conn
        .prepare("select content_id from content order by content_id")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, u32>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| unreadable(OL, &e.to_string()))?;
    let mut want: Vec<u32> = db.tracks.iter().map(|t| t.id).collect();
    want.sort_unstable();
    if ids != want {
        return Err(differs(OL, Diff::Content));
    }
    Ok(ids)
}

/// The analysis files written for each track give back the cues that were encoded.
pub(crate) fn cues(checks: &[CueCheck]) -> Result<(), String> {
    let read = |p: &Path| std::fs::read(p).map_err(|e| e.to_string());
    for c in checks {
        let got = read(&c.dat).and_then(|d| {
            read(&c.ext).and_then(|e| compat_rekordbox_common::anlz::count_cues(&d, Some(&e)))
        });
        match got {
            Ok(n) if n == c.want => {}
            Ok(n) => {
                return Err(format!(
                    "The analysis file of {} holds {n} cues instead of {}. {NEXT}",
                    c.title, c.want
                ));
            }
            Err(e) => {
                return Err(format!(
                    "The analysis files of {} cannot be read back ({e}). {NEXT}",
                    c.title
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{onelibrary, pdb::db::Node};
    use std::path::PathBuf;

    /// `n` tracks T1..Tn and one playlist listing them all.
    fn db(n: u32) -> Db {
        Db {
            tracks: (1..=n)
                .map(|id| Item {
                    id,
                    title: format!("T{id}"),
                    file_path: format!("/Contents/A/B/{id}.mp3"),
                    ..Item::default()
                })
                .collect(),
            nodes: vec![Node {
                id: 1,
                parent: 0,
                name: "P".into(),
                is_folder: false,
                tracks: (1..=n).collect(),
            }],
            analyze_date: "2026-01-01".into(),
            ..Db::default()
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rk-verify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A commit holding `export.pdb` of `pdb` and, when given, `exportLibrary.db` of `one`.
    fn staged(root: &Path, pdb: &Db, one: Option<&Db>) -> Commit {
        let mut c = Commit::new(root, "test").unwrap();
        c.stage(&root.join("export.pdb"), &pdb.encode().unwrap())
            .unwrap();
        if let Some(o) = one {
            let target = root.join("exportLibrary.db");
            onelibrary::write(&Commit::staged_path(&target), "k", o, &Tags::default(), 1).unwrap();
            c.stage_built(&target).unwrap();
        }
        c
    }

    #[test]
    fn what_was_written_reads_back_and_a_difference_is_an_error() {
        let d = dir("read-back");
        let want = db(3);
        let ok = staged(&d, &want, Some(&want));
        assert_eq!(super::staged(&ok, &want, &Tags::default(), "k"), Ok(()));

        // fewer tracks than encoded
        let c = staged(&d, &db(2), Some(&want));
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.starts_with("The Device Library just written lists 2 tracks instead of 3."),
            "{e}"
        );
        assert!(e.contains("Nothing on the stick was changed"), "{e}");

        // the OneLibrary lost a playlist entry
        let mut short = db(3);
        short.nodes[0].tracks.pop();
        let c = staged(&d, &want, Some(&short));
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.starts_with("The OneLibrary just written lists 2 playlist entries instead of 3."),
            "{e}"
        );

        // the playlist tree
        let mut no_list = db(3);
        no_list.nodes.clear();
        let c = staged(&d, &no_list, None);
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.contains("holds 0 playlists and folders instead of 1"),
            "{e}"
        );

        // a title that is not the one encoded
        let mut other = db(3);
        other.tracks[1].title = "Other".into();
        let c = staged(&d, &other, None);
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.contains("does not match what was exported (track T2 differs)"),
            "{e}"
        );

        // not a database at all
        let c = staged(&d, &want, Some(&want));
        std::fs::write(
            Commit::staged_path(&d.join("exportLibrary.db")),
            b"not a database",
        )
        .unwrap();
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.starts_with("The OneLibrary just written cannot be read back"),
            "{e}"
        );

        // a file cut short
        let c = staged(&d, &want, None);
        let p = Commit::staged_path(&d.join("export.pdb"));
        let b = std::fs::read(&p).unwrap();
        std::fs::write(&p, &b[..b.len() / 2]).unwrap();
        let e = super::staged(&c, &want, &Tags::default(), "k").unwrap_err();
        assert!(
            e.starts_with("The Device Library just written cannot be read back"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&d); // cleanup: Windows may still hold a handle
    }

    /// Every sentence a failed check can give has a translation in each language of the plugin (the host's rule: the text is a key,
    /// or matches a key whose `{}` stand for values of at least one character).
    #[test]
    fn every_failure_text_is_translated() {
        let d = dir("texts");
        let a = crate::anlz::encode(&openfader_plugin::library::Analysis::default(), "/x.mp3");
        let (dat, ext) = (d.join("a.DAT"), d.join("a.EXT"));
        std::fs::write(&dat, &a.dat).unwrap();
        std::fs::write(&ext, &a.ext).unwrap();
        let check = |want| CueCheck {
            title: "Song".into(),
            dat: dat.clone(),
            ext: ext.clone(),
            want,
        };
        let mut texts = vec![
            unreadable(DL, "why"),
            differs(DL, Diff::Track("T")),
            differs(OL, Diff::Playlist("P")),
            differs(DL, Diff::Content),
            counts(DL, "tracks", 1, 2),
            counts(OL, "playlists", 1, 2),
            counts(DL, "entries", 1, 2),
            disagree(1, 2),
            cues(&[check(3)]).unwrap_err(),
        ];
        std::fs::write(&ext, b"junk").unwrap();
        texts.push(cues(&[check(0)]).unwrap_err());
        let matches = |key: &str, text: &str| {
            let parts: Vec<&str> = key.split("{}").collect();
            let (first, rest) = parts.split_first().unwrap();
            let Some((last, middle)) = rest.split_last() else {
                return false;
            };
            let Some(mut s) = text.strip_prefix(first).and_then(|t| t.strip_suffix(last)) else {
                return false;
            };
            for part in middle {
                let start = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
                match s.get(start..).and_then(|r| r.find(part)) {
                    Some(i) => s = &s[start + i + part.len()..],
                    None => return false,
                }
            }
            !s.is_empty()
        };
        for lang in ["de", "es", "fr", "it", "nl", "pt"] {
            let file = format!(
                "{}/../device/locales/{lang}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let map: HashMap<String, String> =
                serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
            for t in &texts {
                let found = map.get(t).is_some_and(|v| !v.is_empty())
                    || map
                        .iter()
                        .any(|(k, v)| !v.is_empty() && k.contains("{}") && matches(k, t));
                assert!(found, "{lang} has no translation of: {t}");
            }
        }
        let _ = std::fs::remove_dir_all(&d); // cleanup: Windows may still hold a handle
    }

    #[test]
    fn texts_longer_than_a_row_holds_are_compared_as_written() {
        let d = dir("clip");
        let mut long = db(1);
        long.tracks[0].title = "é".repeat(300);
        let c = staged(&d, &long, None);
        assert_eq!(super::staged(&c, &long, &Tags::default(), "k"), Ok(()));
        let _ = std::fs::remove_dir_all(&d); // cleanup: Windows may still hold a handle
    }

    #[test]
    fn cues_that_do_not_read_back_are_an_error() {
        let d = dir("cues");
        let a = crate::anlz::encode(
            &openfader_plugin::library::Analysis {
                cues: vec![openfader_plugin::library::Cue {
                    hot_cue: 1,
                    time_ms: 1000,
                    loop_end_ms: None,
                    color: None,
                    comment: None,
                }],
                ..Default::default()
            },
            "/x.mp3",
        );
        let (dat, ext) = (d.join("a.DAT"), d.join("a.EXT"));
        std::fs::write(&dat, &a.dat).unwrap();
        std::fs::write(&ext, &a.ext).unwrap();
        let check = |want| CueCheck {
            title: "Song".into(),
            dat: dat.clone(),
            ext: ext.clone(),
            want,
        };
        assert_eq!(cues(&[check(1)]), Ok(()));
        let e = cues(&[check(2)]).unwrap_err();
        assert!(
            e.starts_with("The analysis file of Song holds 1 cues instead of 2."),
            "{e}"
        );
        std::fs::write(&ext, b"junk").unwrap();
        let e = cues(&[check(1)]).unwrap_err();
        assert!(
            e.starts_with("The analysis files of Song cannot be read back"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&d); // cleanup: Windows may still hold a handle
    }
}
