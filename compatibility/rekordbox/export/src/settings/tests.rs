use super::{file::crc16, *};

/// Every key `DeviceConfig::get` / `set` know.
fn keys() -> impl Iterator<Item = String> {
    [
        "device_name",
        "background",
        "categories",
        "sorts",
        "column",
        "hotcue_autoload",
        "jog_custom",
        "auto_apply",
    ]
    .into_iter()
    .map(String::from)
    .chain((1..=8).map(|i| format!("color.{i}")))
    .chain(fields().map(|f| format!("dat.{}", f.id)))
}

/// The rows `export.pdb` tables 17 / 18 hold in the real rekordbox 7 export (observed).
const T17: [[u16; 4]; 22] = [
    [1, 1, 355, 0],
    [5, 6, 261, 0],
    [6, 7, 355, 0],
    [7, 8, 355, 0],
    [8, 9, 355, 0],
    [9, 10, 355, 0],
    [10, 11, 355, 0],
    [13, 15, 355, 0],
    [14, 19, 260, 0],
    [15, 20, 262, 0],
    [16, 21, 355, 0],
    [18, 23, 355, 0],
    [2, 2, 2, 1],
    [3, 3, 3, 2],
    [4, 4, 1, 3],
    [11, 12, 99, 4],
    [17, 5, 99, 5],
    [19, 22, 99, 6],
    [20, 18, 99, 7],
    [27, 26, 611, 8],
    [24, 17, 99, 9],
    [22, 27, 99, 10],
];
const T18: [[u16; 4]; 17] = [
    [1, 6, 1, 0],
    [21, 7, 1, 0],
    [14, 8, 1, 0],
    [8, 9, 1, 0],
    [9, 10, 1, 0],
    [10, 11, 1, 0],
    [15, 13, 1, 0],
    [13, 15, 1, 0],
    [23, 16, 1, 0],
    [22, 17, 1, 0],
    [25, 0, 256, 0],
    [26, 1, 512, 0],
    [2, 2, 768, 0],
    [3, 3, 1024, 0],
    [5, 4, 1280, 0],
    [6, 5, 1536, 0],
    [11, 12, 1792, 0],
];

#[test]
fn crc16_matches_the_xmodem_check_value() {
    assert_eq!(crc16(b"123456789"), 0x31c3);
}

#[test]
fn defaults_have_the_observed_sizes_and_survive_a_parse() {
    let s = Settings::default();
    let files = s.to_bytes();
    let sizes: Vec<_> = files.iter().map(|(n, b)| (*n, b.len())).collect();
    assert_eq!(
        sizes,
        [
            ("MYSETTING.DAT", 148),
            ("MYSETTING2.DAT", 148),
            ("DJMMYSETTING.DAT", 160),
            ("DEVSETTING.DAT", 140)
        ]
    );
    let mut back = Settings::default();
    for (kind, (_, b)) in Kind::ALL.iter().zip(&files) {
        back.parse(*kind, b).unwrap();
    }
    assert_eq!(back, s);
    assert_eq!(back.to_bytes(), files);
}

#[test]
fn damaged_files_are_rejected() {
    let mut s = Settings::default();
    let good = s.to_bytes()[2].1.clone();
    let mut bad = good.clone();
    bad[0x70] ^= 1;
    assert!(s.parse(Kind::Djm, &bad).unwrap_err().contains("checksum"));
    assert!(
        s.parse(Kind::Djm, &good[..good.len() - 1])
            .unwrap_err()
            .contains("size")
    );
    assert!(
        s.parse(Kind::My, &good).is_err(),
        "a DJMMYSETTING file is not a MYSETTING file"
    );
}

#[test]
fn field_tables_are_consistent_and_the_defaults_are_choices() {
    let s = Settings::default();
    let mut ids = std::collections::HashSet::new();
    for f in fields() {
        assert!(ids.insert(f.id), "duplicate id {}", f.id);
        assert!(f.offset < f.kind.data_len(), "{} is outside its file", f.id);
        assert!(
            s.get(f).is_some(),
            "default byte of {} is not a listed choice",
            f.id
        );
        let mut bytes: Vec<_> = f.choices.iter().map(|c| c.0).collect();
        bytes.sort_unstable();
        bytes.dedup();
        assert_eq!(bytes.len(), f.choices.len(), "{} repeats a value", f.id);
    }
    for k in Kind::ALL {
        let mut offs: Vec<_> = k.fields().iter().map(|f| f.offset).collect();
        offs.sort_unstable();
        offs.dedup();
        assert_eq!(offs.len(), k.fields().len(), "{k:?} fields overlap");
    }
}

#[test]
fn editing_one_field_changes_one_byte_and_stays_valid() {
    let base = Settings::default();
    let f = fields().find(|f| f.id == "tempo_range").unwrap();
    let mut s = base.clone();
    s.set(f, 3);
    assert_eq!(s.get(f), Some(3));
    let (a, b) = (base.to_bytes(), s.to_bytes());
    let diff: Vec<_> = a[0]
        .1
        .iter()
        .zip(&b[0].1)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i)
        .collect();
    // the data byte (header 0x68 + offset 36) and the checksum bytes at the end
    assert_eq!(diff[0], 0x68 + 36, "{diff:?}");
    assert!(
        diff.len() <= 3 && diff[1..].iter().all(|&i| i >= a[0].1.len() - 4),
        "{diff:?}"
    );
    assert_eq!(a[1..], b[1..]);
    let mut back = Settings::default();
    back.parse(Kind::My, &b[0].1).unwrap();
    assert_eq!(back.get(f), Some(3));
    assert!(!s.set_byte(f, 0x42));
}

#[test]
fn default_lists_reproduce_the_observed_browse_tables() {
    let c = DeviceConfig::default();
    assert_eq!(c.table17(), T17);
    assert_eq!(c.table18(), T18);
    assert!(T17.iter().all(|r| !browse::label(r[0]).is_empty()));
}

#[test]
fn list_edits_move_rows_between_active_and_inactive() {
    let mut c = DeviceConfig::default();
    c.set("categories", "1,2").unwrap();
    let t = c.table17();
    assert_eq!(t.iter().filter(|r| r[3] != 0).count(), 2);
    assert_eq!(t.last().unwrap()[..2], [2, 2]);
    assert_eq!(t[t.len() - 2][..2], [1, 1]);
    assert_eq!((t.len(), t[t.len() - 2][3], t[t.len() - 1][3]), (22, 1, 2));
    assert!(c.set("categories", "1,1").is_err() && c.set("categories", "99").is_err());
}

#[test]
fn column_follows_the_sort_list() {
    let mut c = DeviceConfig::default();
    c.set("column", "4").unwrap();
    assert_eq!(c.column, Some(4));
    assert!(c.set("column", "0").is_err(), "DEFAULT is not a column");
    assert!(
        c.set("column", "6").is_ok() && c.column.is_none(),
        "6 (GENRE) is not an active sort"
    );
    c.set("column", "4").unwrap();
    c.set("sorts", "0,1,2").unwrap();
    assert_eq!(c.column, None, "removing the sort item clears the column");
}

#[test]
fn every_key_round_trips_and_a_diff_is_what_changed() {
    let base = DeviceConfig::default();
    let mut c = base.clone();
    for k in keys() {
        let v = c.get(&k).unwrap_or_else(|| panic!("no value for {k}"));
        c.set(&k, &v).unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    assert_eq!(c, base);
    c.set("device_name", "BOOTH").unwrap();
    c.set("background", "3").unwrap();
    c.set("color.2", "Red hot").unwrap();
    c.set("dat.sync", "129").unwrap();
    let changed: Vec<_> = keys().filter(|k| c.get(k) != base.get(k)).collect();
    assert_eq!(
        changed,
        ["device_name", "background", "color.2", "dat.sync"]
    );
    assert!(
        c.set("background", "9").is_err()
            && c.set("dat.sync", "5").is_err()
            && c.set("nope", "1").is_err()
    );
}

#[test]
fn files_include_the_jog_image_only_when_chosen() {
    let mut c = DeviceConfig::default();
    assert_eq!(c.files().len(), 4);
    c.jog_image = vec![0xff, 0xd8, 0xff];
    assert_eq!(c.files().len(), 4);
    c.jog_custom = true;
    assert_eq!(c.files().last().unwrap().0, JOG_IMAGE_FILE);
}

#[test]
fn write_then_read_returns_the_edited_settings() {
    let dir = std::env::temp_dir().join(format!("rk-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut c = DeviceConfig::default();
    c.set("dat.language", "131").unwrap();
    c.set("dat.waveform_color", "4").unwrap();
    c.write_files(&dir).unwrap();
    let back = DeviceConfig::read(&dir.join(DIR)).unwrap();
    assert_eq!(back.get("dat.language").unwrap(), "131");
    assert_eq!(back.get("dat.waveform_color").unwrap(), "4");
    assert_eq!(back.settings, c.settings);
    let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-settings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn from_values_gives_the_files_of_the_same_edits_made_one_by_one() {
    let v = values(&[
        ("dat.language", "131"),
        ("dat.tempo_range", "131"),
        ("dat.crossfader_curve", "130"),
        ("dat.waveform_color", "4"),
        ("device_name", "BOOTH"),
        ("background", "3"),
        ("color.2", "Red hot"),
        ("categories", "1,2"),
        ("sorts", "0,1,2,3,6"),
        ("column", "6"),
        ("hotcue_autoload", "0"),
    ]);
    let mut by_hand = DeviceConfig::default();
    for (k, x) in [
        ("dat.language", "131"),
        ("dat.tempo_range", "131"),
        ("dat.crossfader_curve", "130"),
        ("dat.waveform_color", "4"),
        ("device_name", "BOOTH"),
        ("background", "3"),
        ("color.2", "Red hot"),
        ("categories", "1,2"),
        ("sorts", "0,1,2,3,6"),
        ("column", "6"),
        ("hotcue_autoload", "0"),
    ] {
        by_hand.set(k, x).unwrap();
    }
    let c = DeviceConfig::from_values(&v);
    assert_eq!(c, by_hand);
    assert_eq!(
        c.column,
        Some(6),
        "column is checked against the final sorts"
    );
    assert_eq!(c.files(), by_hand.files());
    assert_eq!(c.table17(), by_hand.table17());
    assert_eq!(c.table18(), by_hand.table18());
    assert_eq!(
        DeviceConfig::from_values(&BTreeMap::new()),
        DeviceConfig::default()
    );
    assert_eq!(
        DeviceConfig::from_values(&BTreeMap::new()).files(),
        Settings::default().to_bytes(),
        "no values: the observed export files"
    );
}

#[test]
fn from_values_ignores_what_it_cannot_use() {
    let v = values(&[
        ("background", "9"),
        ("dat.sync", "5"),
        ("dat.nope", "1"),
        ("nope", "1"),
        ("written.dat", "abcd"),
        ("categories", "1,1"),
        ("auto_apply", "yes"),
        ("color.1", ""),
        ("jog_image", "/definitely/not/here.jpg"),
    ]);
    assert_eq!(DeviceConfig::from_values(&v), DeviceConfig::default());
}

#[test]
fn the_jog_image_value_is_a_jpeg_file() {
    let dir = temp("jog");
    let jpg = dir.join("a.jpg");
    std::fs::write(&jpg, [0xff, 0xd8, 0xff, 0xe0, 1, 2]).unwrap();
    let png = dir.join("a.png");
    std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
    let path = |p: &std::path::Path| p.to_str().unwrap().to_owned();
    let c = DeviceConfig::from_values(&values(&[("jog_custom", "1"), ("jog_image", &path(&jpg))]));
    assert_eq!(c.jog_image, [0xff, 0xd8, 0xff, 0xe0, 1, 2]);
    assert_eq!(
        c.files().last().unwrap(),
        &(JOG_IMAGE_FILE, c.jog_image.clone())
    );
    let c = DeviceConfig::from_values(&values(&[("jog_custom", "1"), ("jog_image", &path(&png))]));
    assert!(c.jog_image.is_empty() && c.files().len() == 4);
    let _ = std::fs::remove_dir_all(&dir); // cleanup: Windows may still hold a handle
}

#[test]
fn write_then_read_round_trips_on_a_folder_and_the_fingerprints_agree() {
    let root = temp("rw");
    let empty = read(&root, &Value::Null).unwrap();
    assert!(empty.values.is_empty() && empty.fingerprint.is_empty());
    let v = values(&[("dat.language", "131"), ("dat.crossfader_curve", "130")]);
    let written = write(&root, &v, &Value::Null).unwrap();
    let back = read(&root, &Value::Null).unwrap();
    assert_eq!(written.fingerprint, back.fingerprint);
    assert!(!back.fingerprint.is_empty());
    assert_eq!(back.values["dat.language"], "131");
    assert_eq!(back.values["dat.crossfader_curve"], "130");
    assert_eq!(
        back.values["dat.sync"],
        DeviceConfig::default().get("dat.sync").unwrap()
    );
    assert_eq!(
        back.values.len(),
        fields().count(),
        "all four files were written"
    );
    let dir = root.join(DIR);
    let names: Vec<_> = Kind::ALL.iter().map(|k| k.file_name()).collect();
    for n in &names {
        assert!(dir.join(n).is_file(), "{n}");
    }
    assert_eq!(
        DeviceConfig::read(&dir).unwrap(),
        DeviceConfig::from_values(&v)
    );
    // a write is explicit: it ignores `auto_apply`
    let off = write(
        &root,
        &values(&[("auto_apply", "0"), ("dat.language", "132")]),
        &Value::Null,
    )
    .unwrap();
    assert_eq!(
        read(&root, &Value::Null).unwrap().values["dat.language"],
        "132"
    );
    assert_ne!(off.fingerprint, written.fingerprint);
    // the player changes a file
    let mut changed = DeviceConfig::read(&dir).unwrap();
    let f = fields().find(|f| f.id == "sync").unwrap();
    changed.settings.set(f, 1);
    std::fs::write(dir.join("MYSETTING.DAT"), &changed.settings.to_bytes()[0].1).unwrap();
    let now = read(&root, &Value::Null).unwrap();
    assert_ne!(now.fingerprint, off.fingerprint);
    assert_eq!(now.values["dat.sync"], "129");
    // a file the device does not have has no values (real exports carry no DEVSETTING.DAT)
    std::fs::remove_file(dir.join("DEVSETTING.DAT")).unwrap();
    let now = read(&root, &Value::Null).unwrap();
    assert!(!now.values.contains_key("dat.waveform_color"));
    assert!(now.values.contains_key("dat.language"));
    // a damaged one is an error
    std::fs::write(dir.join("MYSETTING2.DAT"), b"junk").unwrap();
    assert!(read(&root, &Value::Null).is_err());
    let _ = std::fs::remove_dir_all(&root); // cleanup: Windows may still hold a handle
}

#[test]
fn writing_a_chosen_jog_image_puts_it_next_to_the_settings() {
    let root = temp("wjog");
    let jpg = root.join("pic.jpg");
    std::fs::write(&jpg, [0xff, 0xd8, 0xff, 1]).unwrap();
    write(
        &root,
        &values(&[("jog_custom", "1"), ("jog_image", jpg.to_str().unwrap())]),
        &Value::Null,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.join(DIR).join(JOG_IMAGE_FILE)).unwrap(),
        [0xff, 0xd8, 0xff, 1]
    );
    let _ = std::fs::remove_dir_all(&root); // cleanup: Windows may still hold a handle
}
