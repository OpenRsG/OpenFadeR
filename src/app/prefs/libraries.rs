//! LIBRARIES category (Preferences > Libraries): the libraries synced into the Collection ([`crate::app::connect`]). Each
//! connection lists its source, track count and last sync with its mode (IMPORT reads the source, TWO-WAY also writes our
//! edits back), the conflict rule, SYNC NOW and DISCONNECT (the tracks stay in the Collection). Below, the libraries found in
//! other apps' usual places with CONNECT, and ADD… (a library file or a folder). Row keys: `conn.<id>.mode`, `.wins`, `.act`;
//! `det.<n>`; `add`.

use super::{Prefs, Row};
use crate::app::{connect, library};
use openfader::library::connect::{Mode, Wins};

/// No chip is lit: a row of buttons.
const NONE: usize = usize::MAX;

pub fn rows(_: &Prefs) -> Vec<Row> {
    let mut rows = vec![Row::header("CONNECTED")];
    let views = connect::views();
    if views.is_empty() {
        rows.push(Row::info("", "No library is connected."));
    }
    for v in views {
        let c = &v.conn;
        let id = c.id;
        let source = if c.plugin.is_empty() {
            "folder".to_owned()
        } else {
            c.plugin.clone()
        };
        rows.push(Row::info(
            &c.label,
            &format!("{source}: {}", c.path.display()),
        ));
        let state = if v.busy {
            "syncing…".to_owned()
        } else {
            format!("synced {}", connect::ago(c.last_sync))
        };
        rows.push(Row::info("", &format!("{} tracks, {state}", v.tracks)));
        rows.push(Row::chips(
            &format!("conn.{id}.mode"),
            "Mode",
            &["IMPORT", "TWO-WAY"],
            c.mode as usize,
        ));
        rows.push(Row::chips(
            &format!("conn.{id}.wins"),
            "On conflict",
            &["SOURCE WINS", "OURS WINS", "NEWEST WINS"],
            c.wins as usize,
        ));
        rows.push(Row::chips(
            &format!("conn.{id}.act"),
            "",
            &["SYNC NOW", "DISCONNECT"],
            NONE,
        ));
    }
    rows.push(Row::header("DETECTED"));
    let found = connect::found();
    if found.is_empty() {
        rows.push(Row::info(
            "",
            "No library of another app found in its usual place.",
        ));
    }
    for (i, f) in found.iter().enumerate() {
        rows.push(
            Row::chips(&format!("det.{i}"), &f.label, &["CONNECT"], NONE)
                .hint(&f.path.display().to_string()),
        );
    }
    rows.push(Row::chips("add", "Add", &["FILE…", "FOLDER…"], NONE));
    rows
}

pub fn choose(_: &mut Prefs, key: &str, i: usize) -> bool {
    if let Some(n) = key.strip_prefix("det.").and_then(|n| n.parse().ok()) {
        connect::connect_found(n);
        return true;
    }
    if key == "add" {
        library::ask_connect(i == 1);
        return true;
    }
    let Some(rest) = key.strip_prefix("conn.") else {
        return false;
    };
    let Some((id, what)) = rest.split_once('.') else {
        return false;
    };
    let Ok(id) = id.parse() else { return false };
    match what {
        "mode" => connect::set_mode(id, if i == 1 { Mode::TwoWay } else { Mode::Import }),
        "wins" => connect::set_wins(id, [Wins::Source, Wins::Ours, Wins::Newest][i.min(2)]),
        "act" if i == 0 => connect::sync_now(id),
        "act" => connect::disconnect(id),
        _ => return false,
    }
    true
}
