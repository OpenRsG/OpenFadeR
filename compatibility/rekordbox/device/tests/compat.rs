//! `players.json` (read by the app's stick checker) and `COMPATIBILITY.md` (read by people) say the same about the same models, and
//! every library a player is said to read is one `device_check` reports.

use openfader_plugin::library::Players;
use std::collections::BTreeMap;

const PLAYERS: &str = include_str!("../players.json");
const TABLE: &str = include_str!("../COMPATIBILITY.md");

fn players() -> Players {
    serde_json::from_str(PLAYERS).expect("players.json parses as Players")
}

#[test]
fn compatibility_lists_every_player_and_library() {
    let p = players();
    assert_eq!(p.version, 1);
    assert_eq!(p.players.len(), 10);
    // The libraries a check reports are the ones the players are matched by.
    let dir = std::env::temp_dir().join(format!("rk-compat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reported: Vec<String> =
        compat_rekordbox_export::device_check(&dir, &BTreeMap::new(), &serde_json::Value::Null)
            .unwrap()
            .libraries
            .into_iter()
            .map(|l| l.name)
            .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(reported, ["Device Library", "OneLibrary"]);

    let rows: Vec<&str> = TABLE.lines().filter(|l| l.starts_with('|')).collect();
    for m in &p.players {
        assert!(!m.reads.is_empty(), "{} reads nothing", m.model);
        for lib in &m.reads {
            assert!(
                reported.contains(lib),
                "{} reads {lib}, which no check reports",
                m.model
            );
        }
        assert!(
            ["confirmed", "to_measure"].contains(&m.status.as_str()),
            "{}",
            m.model
        );
        let row = rows
            .iter()
            .find(|r| r.starts_with(&format!("| {} |", m.model)))
            .unwrap_or_else(|| panic!("COMPATIBILITY.md has no row for {}", m.model));
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        // | Model | Status | Reads | File systems | Firmware | Recorded | Source |
        assert!(
            ["Tested", "Not tested", "Known problem"].contains(&cells[2]),
            "{row}"
        );
        for lib in &m.reads {
            assert!(
                cells[3].contains(lib.as_str()),
                "{} row does not say it reads {lib}",
                m.model
            );
        }
        assert!(cells[7].contains("owner's study"), "{row}");
        assert_eq!(
            m.status == "to_measure",
            cells[3].contains("to measure"),
            "{row}"
        );
    }
    // No player has been tested with OpenFadeR's exports: no row may say so.
    assert!(!rows
        .iter()
        .skip(2)
        .any(|r| r.split('|').nth(2).is_some_and(|c| c.trim() == "Tested")));
    assert!(TABLE.contains("No player has been tested"));
    // Every row of the table is a player of players.json.
    let listed = rows
        .iter()
        .filter_map(|r| r.split('|').nth(1))
        .map(str::trim)
        .filter(|m| p.players.iter().any(|x| x.model == *m))
        .count();
    assert_eq!(listed, p.players.len());
}

#[test]
fn the_booth_presets_name_known_players() {
    let p = players();
    let ids: Vec<(&str, &str)> = p
        .presets
        .iter()
        .map(|b| (b.id.as_str(), b.label.as_str()))
        .collect();
    assert_eq!(
        ids,
        [
            ("older", "Older players"),
            ("mixed", "Mixed booth"),
            ("newest", "Newest only")
        ]
    );
    for b in &p.presets {
        for m in &b.models {
            assert!(
                p.players.iter().any(|x| &x.model == m),
                "{m} in {}",
                b.label
            );
        }
    }
    let by = |id: &str| {
        p.presets
            .iter()
            .find(|b| b.id == id)
            .unwrap()
            .models
            .clone()
    };
    assert_eq!(
        by("older"),
        ["CDJ-2000NXS2", "CDJ-2000 Nexus", "XDJ-1000MK2"]
    );
    assert_eq!(by("mixed"), ["CDJ-2000NXS2", "CDJ-3000", "CDJ-3000X"]);
    assert_eq!(
        by("newest"),
        ["CDJ-3000X", "OPUS-QUAD", "OMNIS-DUO", "XDJ-AZ"]
    );
}
