//! Our exporter's constants checked against a real rekordbox 7 export: the OneLibrary schema and constant tables, the settings
//! files and the PQT2 header. Skips without `OPENFADER_USB` (and, for the database, the OneLibrary key). The USB is only read;
//! the database is copied to a temp dir first because the real file is in WAL mode.

mod common;

use openfader_plugin::library::Analysis;
use rekordbox_common::{anlz as read_anlz, on_device};
use rekordbox_export::{
    anlz, onelibrary,
    pdb::{
        db::{Db, Tags},
        read,
    },
    profile::{self, Profile},
    settings::{self, DeviceConfig},
};
use rusqlite::{Connection, OpenFlags, types::Value};
use std::path::Path;

fn open(path: &Path, key: &str) -> Connection {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.pragma_update(None, "key", key).unwrap();
    c
}

/// Every row of `sql` as text-ish values.
fn dump(c: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn onelibrary_schema_and_constant_tables_match_the_real_file() {
    let (Some(root), Some(key)) = (common::usb_root(), common::onelibrary_key()) else {
        return;
    };
    let dir = common::fresh_dir("export-usb-db");
    let real = dir.join("real.db");
    std::fs::copy(root.join("PIONEER/rekordbox/exportLibrary.db"), &real).unwrap();
    for ext in ["-wal", "-shm"] {
        let side = root.join(format!("PIONEER/rekordbox/exportLibrary.db{ext}"));
        if side.is_file() {
            std::fs::copy(side, dir.join(format!("real.db{ext}"))).unwrap();
        }
    }
    let ours = dir.join("ours.db");
    onelibrary::write(
        &ours,
        &key,
        &Db {
            analyze_date: "2026-01-01".into(),
            ..Db::default()
        },
        &Tags::default(),
        1,
    )
    .unwrap();
    let (real, ours) = (open(&real, &key), open(&ours, &key));
    assert_eq!(
        dump(
            &ours,
            "select type, name, tbl_name, sql from sqlite_master order by rowid"
        ),
        dump(
            &real,
            "select type, name, tbl_name, sql from sqlite_master order by rowid"
        ),
        "schema"
    );
    for table in ["menuItem", "category", "sort", "color"] {
        let sql = format!("select * from {table} order by 1");
        assert_eq!(dump(&ours, &sql), dump(&real, &sql), "{table}");
    }
    let _ = std::fs::remove_dir_all(dir); // cleanup: Windows may still hold a handle
}

#[test]
fn settings_files_equal_the_real_ones() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let mut seen = 0;
    for (name, bytes) in DeviceConfig::default().files() {
        let Ok(real) = std::fs::read(root.join(settings::DIR).join(name)) else {
            continue;
        }; // a stick the settings were never written to lacks some of the files
        assert_eq!(bytes, real, "{name}");
        seen += 1;
    }
    assert_eq!(
        seen, 4,
        "DEVSETTING, DJMMYSETTING, MYSETTING, MYSETTING2 of the real stick"
    );
}

#[test]
fn djprofile_round_trips_the_real_one_and_ours_has_the_same_shape() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let Ok(real) = std::fs::read(root.join("PIONEER").join(profile::FILE)) else {
        return; // a stick rekordbox never wrote the profile to
    };
    let p = Profile::parse(&real).expect("the real djprofile.nxs parses");
    assert_eq!(p.to_bytes().as_slice(), real.as_slice(), "round trip");
    assert!(!p.name.is_empty() && p.millis > 1_000_000_000_000);
    let d = common::fresh_dir("export-usb-nxs");
    assert!(profile::write(&d, "Test DJ").unwrap());
    let ours = std::fs::read(d.join(profile::FILE)).unwrap();
    assert_eq!(ours.len(), real.len());
    assert_eq!(ours[0x0c..0x1c], real[0x0c..0x1c], "the zero run");
    assert_eq!(Profile::parse(&ours).unwrap().name, "Test DJ");
    let _ = std::fs::remove_dir_all(d); // cleanup: Windows may still hold a handle
}

/// `(offset, len)` of the first section `tag` of an ANLZ file.
fn section(f: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let mut o = 28;
    while o + 12 <= f.len() {
        let len = u32::from_be_bytes(f[o + 8..o + 12].try_into().unwrap()) as usize;
        if &f[o..o + 4] == tag {
            return Some((o, len));
        }
        o += len;
    }
    None
}

#[test]
fn pqt2_header_equals_the_real_one_and_the_fractions_are_thousandths() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let (db, _) = read::open(&root, &DeviceConfig::default()).unwrap();
    let mut checked = 0;
    for t in db.tracks {
        let dat = on_device(&root, &t.analyze_path);
        let Ok(real) = std::fs::read(dat.with_extension("EXT")) else {
            continue;
        };
        let Some((at, len)) = section(&real, b"PQT2") else {
            continue;
        }; // rekordbox leaves it out of some tracks
        let beats = read_anlz::load_track(&dat).unwrap().beats;
        let files = anlz::encode(
            &Analysis {
                beats: beats.clone(),
                ..Analysis::default()
            },
            "/x.mp3",
        );
        let (our_at, our_len) = section(&files.ext, b"PQT2").expect("our PQT2");
        assert_eq!(
            (our_len, &files.ext[our_at..our_at + 56]),
            (len, &real[at..at + 56]),
            "{}",
            dat.display()
        );
        let n = beats.len();
        assert!(
            real[at + 56..at + len]
                .chunks(2)
                .all(|c| u16::from_be_bytes([c[0], c[1]]) < 1000),
            "fractions are 0..=999"
        );
        assert_eq!(real[at + 56..at + len].len(), 2 * n);
        checked += 1;
    }
    assert!(checked > 0, "no PQT2 on this USB");
}

/// What the waveform writer (`anlz::wave`) reproduces holds in every real file: PWAV heights span exactly 2..=25 with whiteness
/// <= 5, PWV2 reaches 15; the covers are 80×80 and 240×240; table 19 of `export.pdb` holds one row with the track count.
#[test]
fn real_waveform_ranges_covers_and_library_row() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let mut seen = 0;
    for p in std::fs::read_dir(root.join("PIONEER/USBANLZ")).unwrap().flatten() {
        for t in std::fs::read_dir(p.path()).unwrap().flatten() {
            let dat = std::fs::read(t.path().join("ANLZ0000.DAT")).unwrap();
            let body = |tag: &[u8; 4]| {
                let (o, len) = section(&dat, tag).unwrap();
                let header = u32::from_be_bytes(dat[o + 4..o + 8].try_into().unwrap()) as usize;
                dat[o + header..o + len].to_vec()
            };
            let pwav = body(b"PWAV");
            let h: Vec<u8> = pwav.iter().map(|v| v & 31).collect();
            assert_eq!((h.iter().min(), h.iter().max()), (Some(&2), Some(&25)));
            assert!(pwav.iter().all(|v| v >> 5 <= 5));
            assert_eq!(body(b"PWV2").iter().max(), Some(&15));
            seen += 1;
        }
    }
    assert!(seen > 0);
    let db = std::fs::read(root.join("PIONEER/rekordbox/export.pdb")).unwrap();
    let info = rekordbox_common::pdb::rows(&db, 19).unwrap();
    let tracks = rekordbox_common::pdb::rows(&db, 0).unwrap().len() as u32;
    assert_eq!(info.len(), 1);
    assert_eq!(rekordbox_common::pdb::u32_at(&db, info[0] + 4).unwrap(), tracks);
    assert_eq!(rekordbox_common::pdb::u16_at(&db, info[0]).unwrap(), 0x0280);
    for (name, side) in [("a1.jpg", 80), ("a1_m.jpg", 240)] {
        let img = image::open(root.join("PIONEER/Artwork/00001").join(name)).unwrap();
        assert_eq!((img.width(), img.height()), (side, side), "{name}");
    }
}
