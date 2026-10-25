//! Builds a SQLCipher test library from the local sample pack (`.local/extract/spp/` at the repo root).
//! Usage: `cargo run --release -p rekordbox-db --example make_fixture -- <out_dir>`;
//! key from `.local/db-key`. Table/column names are as observed in a real master.db; the rows are synthetic.
#![allow(dead_code)] // tests/samples.rs includes this file as a module and uses only some items

use rusqlite::{Connection, params};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The repository root (this crate lives in `crates/rekordbox-db`).
pub const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

pub fn spp_dir() -> PathBuf {
    Path::new(ROOT).join(".local/extract/spp")
}

pub fn key() -> Option<String> {
    fs::read_to_string(Path::new(ROOT).join(".local/db-key"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// `(id, location, bpm_x100)` of each `<TRACK ... location=.. bpm=..>` in the pack XML.
pub fn xml_tracks(spp: &Path) -> Result<Vec<(String, String, i64)>, String> {
    let xml = fs::read_to_string(spp.join("sampler_pack.xml")).map_err(|e| e.to_string())?;
    let attr = |tag: &str, name: &str| {
        let i = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(tag[i..].split('"').next()?.to_string())
    };
    let mut v = Vec::new();
    for tag in xml.split("<TRACK ").skip(1) {
        let tag = format!(" {}", tag.split("/>").next().unwrap_or(""));
        let tag = tag.as_str();
        if let (Some(id), Some(loc), Some(bpm)) =
            (attr(tag, "id"), attr(tag, "location"), attr(tag, "bpm"))
        {
            let bpm: f64 = bpm.parse().map_err(|_| format!("bad bpm {bpm}"))?;
            v.push((id, loc, (bpm * 100.0).round() as i64));
        }
    }
    Ok(v)
}

/// Seconds of PCM audio from the RIFF `fmt `/`data` chunks.
fn wav_seconds(b: &[u8]) -> Result<f64, String> {
    let (mut p, mut rate, mut data) = (12, 0u32, None);
    while p + 8 <= b.len() {
        let size = u32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap()) as usize;
        match &b[p..p + 4] {
            b"fmt " if p + 20 <= b.len() => {
                rate = u32::from_le_bytes(b[p + 16..p + 20].try_into().unwrap())
            }
            b"data" => data = Some(size.min(b.len() - p - 8)),
            _ => {}
        }
        p += 8 + size + (size & 1);
    }
    match (rate, data) {
        (r, Some(d)) if r > 0 => Ok(d as f64 / r as f64),
        _ => Err("no fmt/data chunk".into()),
    }
}

fn file_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

pub fn build(spp: &Path, out: &Path, key: &str) -> Result<(), String> {
    let e = |e: &dyn std::fmt::Display| e.to_string();
    let db_path = out.join("master.db");
    if db_path.exists() {
        return Err(format!("{} already exists", db_path.display()));
    }
    let tracks = xml_tracks(spp)?;

    // Map DAT -> track via the DAT's PPTH filename.
    let mut dats = Vec::new();
    for d in fs::read_dir(spp.join("Anlz")).map_err(|x| e(&x))? {
        let p = d.map_err(|x| e(&x))?.path();
        let a = rekordbox_common::anlz::parse(&fs::read(&p).map_err(|x| e(&x))?)?;
        let name = file_name(a.path.as_deref().ok_or("DAT without PPTH")?).to_string();
        dats.push((name, p));
    }

    fs::create_dir_all(out).map_err(|x| e(&x))?;
    let db = Connection::open(&db_path).map_err(|x| e(&x))?;
    db.pragma_update(None, "key", key).map_err(|x| e(&x))?;
    db.execute_batch(
        "create table djmdContent(ID VARCHAR primary key, FolderPath VARCHAR, FileNameL VARCHAR, Title VARCHAR, ArtistID VARCHAR, AlbumID VARCHAR, KeyID VARCHAR, GenreID VARCHAR,
                                  BPM INTEGER, Length INTEGER, AnalysisDataPath VARCHAR, ImagePath VARCHAR, Rating INTEGER, Commnt VARCHAR, StockDate VARCHAR, DateCreated VARCHAR,
                                  BitRate INTEGER, SampleRate INTEGER, rb_local_deleted INTEGER default 0);
         create table djmdArtist(ID VARCHAR primary key, Name VARCHAR, rb_local_deleted INTEGER default 0);
         create table djmdAlbum(ID VARCHAR primary key, Name VARCHAR, rb_local_deleted INTEGER default 0);
         create table djmdKey(ID VARCHAR primary key, ScaleName VARCHAR, rb_local_deleted INTEGER default 0);
         create table djmdGenre(ID VARCHAR primary key, Name VARCHAR, rb_local_deleted INTEGER default 0);
         create table djmdPlaylist(ID VARCHAR primary key, Seq INTEGER, Name VARCHAR, Attribute INTEGER, ParentID VARCHAR, SmartList TEXT, rb_local_deleted INTEGER default 0);
         create table djmdSongPlaylist(ID VARCHAR primary key, PlaylistID VARCHAR, ContentID VARCHAR, TrackNo INTEGER, rb_local_deleted INTEGER default 0);
         create table djmdHistory(ID VARCHAR primary key, Seq INTEGER, Name VARCHAR, Attribute INTEGER, ParentID VARCHAR, rb_local_deleted INTEGER default 0);
         create table djmdSongHistory(ID VARCHAR primary key, HistoryID VARCHAR, ContentID VARCHAR, TrackNo INTEGER, rb_local_deleted INTEGER default 0);
         insert into djmdArtist values ('1', 'Groove Circuit Factory', 0);
         insert into djmdAlbum values ('1', 'Sample Pack Vol. 1', 0);
         insert into djmdKey values ('1', '8A', 0);
         insert into djmdGenre values ('1', 'House', 0), ('2', 'Breaks', 0);
         insert into djmdPlaylist(ID, Seq, Name, Attribute, ParentID) values ('1', 0, 'Packs', 1, 'root'), ('2', 0, 'Breaks', 0, '1'), ('3', 1, 'House', 0, 'root');
         insert into djmdHistory values ('h1', 0, '2026', 1, 'root', 0), ('h2', 0, 'Friday', 0, 'h1', 0), ('h3', 1, 'HISTORY 002', 0, 'root', 0), ('h4', 2, 'Gone', 0, 'root', 1);",
    )
    .map_err(|x| e(&x))?;
    let smart = |seq: i64, name: &str, any: bool, conds: &[(&str, u8, &str, &str)]| {
        let c: String = conds
            .iter()
            .map(|(p, o, l, r)| format!("<CONDITION PropertyName=\"{p}\" Operator=\"{o}\" ValueUnit=\"\" ValueLeft=\"{l}\" ValueRight=\"{r}\"/>"))
            .collect();
        let xml = format!(
            "<NODE Id=\"-1\" LogicalOperator=\"{}\" AutomaticUpdate=\"0\">{c}</NODE>",
            if any { 2 } else { 1 }
        );
        db.execute(
            "insert into djmdPlaylist(ID, Seq, Name, Attribute, ParentID, SmartList) values (?1, ?2, ?3, 4, 'root', ?4)",
            params![format!("s{seq}"), seq + 2, name, xml],
        )
    };
    smart(0, "Mid tempo", false, &[("bpm", 5, "12400", "14000")]).map_err(|x| e(&x))?;
    smart(
        1,
        "House 124-140",
        false,
        &[("name", 8, "house", ""), ("bpm", 5, "12400", "14000")],
    )
    .map_err(|x| e(&x))?;
    smart(
        2,
        "Techno or fast",
        true,
        &[("name", 1, "Techno1", ""), ("bpm", 3, "14500", "")],
    )
    .map_err(|x| e(&x))?;

    let (mut breaks, mut house) = (Vec::new(), Vec::new());
    for (id, loc, bpm) in &tracks {
        let wav = spp.join(loc);
        let fname = file_name(loc);
        let stem = fname.trim_end_matches(".wav");
        let secs = wav_seconds(&fs::read(&wav).map_err(|x| e(&x))?)?;
        let (_, dat) = dats
            .iter()
            .find(|(n, _)| n == fname)
            .ok_or(format!("no DAT for {fname}"))?;
        let rel = format!("/PIONEER/USBANLZ/P001/{id:0>8}/ANLZ0000.DAT");
        let dst = out.join("share").join(rel.trim_start_matches('/'));
        fs::create_dir_all(dst.parent().unwrap()).map_err(|x| e(&x))?;
        fs::copy(dat, &dst).map_err(|x| e(&x))?;
        let abs = fs::canonicalize(&wav).map_err(|x| e(&x))?;
        let genre = if stem.starts_with("Breaks") { "2" } else { "1" };
        db.execute(
            "insert into djmdContent values (?1, ?2, ?3, ?4, '1', '1', '1', ?5, ?6, ?7, ?8, NULL, 0, '', '2026-01-01', '2026-01-01', 1411, 44100, 0)",
            params![id, abs.to_string_lossy(), fname, stem, genre, bpm, secs.round() as i64, rel],
        )
        .map_err(|x| e(&x))?;
        if stem.starts_with("Breaks") {
            breaks.push(id.clone())
        } else if stem.starts_with("House") {
            house.push(id.clone())
        }
    }
    // Unanalysed (no AnalysisDataPath) and soft-deleted rows; the deleted one also sits in 'Breaks'.
    db.execute_batch(
        "insert into djmdContent values ('90', '/nonexistent/Unanalysed.wav', 'Unanalysed.wav', 'Unanalysed', '1', NULL, NULL, NULL, 12800, 10, NULL, NULL, 0, '', NULL, NULL, 0, 0, 0),
                                        ('91', '/nonexistent/Deleted.wav', 'Deleted.wav', 'Deleted', '1', '1', '1', '2', 12800, 10, '/PIONEER/USBANLZ/P001/00000091/ANLZ0000.DAT', NULL, 0, '', NULL, NULL, 0, 0, 1);",
    )
    .map_err(|x| e(&x))?;
    breaks.push("91".into());
    breaks.reverse(); // TrackNo order differs from insertion order on purpose
    for (pl, ids) in [("2", &breaks), ("3", &house)] {
        for (n, id) in ids.iter().enumerate() {
            db.execute(
                "insert into djmdSongPlaylist values (?1, ?2, ?3, ?4, 0)",
                params![format!("{pl}-{id}"), pl, id, n as i64 + 1],
            )
            .map_err(|x| e(&x))?;
        }
    }
    // Histories: 'Friday' plays House in reverse; 'HISTORY 002' holds House[0] plus a soft-deleted row.
    for (n, id) in house.iter().rev().enumerate() {
        db.execute(
            "insert into djmdSongHistory values (?1, 'h2', ?2, ?3, 0)",
            params![format!("h2-{id}"), id, n as i64 + 1],
        )
        .map_err(|x| e(&x))?;
    }
    db.execute_batch(&format!(
        "insert into djmdSongHistory values ('h3-a', 'h3', '{}', 1, 0), ('h3-b', 'h3', '{}', 2, 1);",
        house[0],
        house.get(1).unwrap_or(&house[0])
    ))
    .map_err(|x| e(&x))?;
    Ok(())
}

fn main() -> Result<(), String> {
    let out = std::env::args()
        .nth(1)
        .ok_or("usage: make_fixture <out_dir>")?;
    let key = key().ok_or("create .local/db-key")?;
    build(&spp_dir(), Path::new(&out), &key)?;
    println!("fixture written to {out} (master.db + share/)");
    Ok(())
}
