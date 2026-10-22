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
    let mut rows = Vec::new();
    let views = connect::views();
    if views.is_empty() {
        rows.push(Row::header(tr!("CONNECTED")));
        rows.push(Row::info("", tr!("No library is connected.")));
    }
    for v in views {
        let c = &v.conn;
        let id = c.id;
        let source = if c.plugin.is_empty() {
            tr!("folder").to_owned()
        } else {
            c.plugin.clone()
        };
        let state = if v.busy {
            tr!("syncing…").to_owned()
        } else {
            tr!("synced {}", connect::ago(c.last_sync))
        };
        rows.push(Row::header(&c.label));
        rows.push(Row::info(
            tr!("SOURCE"),
            &format!("{source}: {}", c.path.display()),
        ));
        rows.push(Row::info(tr!("TRACKS"), &format!("{}, {state}", v.tracks)));
        rows.push(Row::chips(
            &format!("conn.{id}.mode"),
            tr!("MODE"),
            &[tr!("IMPORT"), tr!("TWO-WAY")],
            c.mode as usize,
        ));
        rows.push(Row::chips(
            &format!("conn.{id}.wins"),
            tr!("ON CONFLICT"),
            &[tr!("SOURCE WINS"), tr!("OURS WINS"), tr!("NEWEST WINS")],
            c.wins as usize,
        ));
        rows.push(Row::chips(
            &format!("conn.{id}.act"),
            "",
            &[tr!("SYNC NOW"), tr!("DISCONNECT")],
            NONE,
        ));
    }
    rows.push(Row::header(tr!("DETECTED")));
    let found = connect::found();
    if found.is_empty() {
        let text = if openfader::plugin::registry().libraries.is_empty() {
            tr!("No library plugin is on: switch one on in EXTENSIONS, then restart, to find other apps' libraries.")
        } else {
            tr!("No library of another app found in its usual place.")
        };
        rows.push(Row::info("", text));
    }
    for (i, f) in found.iter().enumerate() {
        rows.push(
            Row::chips(&format!("det.{i}"), &f.label, &[tr!("CONNECT")], NONE)
                .hint(&f.path.display().to_string()),
        );
    }
    rows.push(Row::chips("add", tr!("ADD"), &[tr!("FILE…"), tr!("FOLDER…")], NONE));
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
