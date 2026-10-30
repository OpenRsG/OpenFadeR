//! `openfader verify <mount> [--players "CDJ-3000X,CDJ-2000NXS2"] [--json]`: the stick checker without a window and without audio.
//! It reads the stick and says whether it is ready for a set, which libraries it holds, what is wrong and which players read it.
//! It never writes: a change a pulled stick cut off is reported, not recovered (the app does that when the stick is mounted).
//!
//! Exit codes: 0 ready, 1 needs fixing (or the stick is empty), 2 unreadable, 3 it could not be checked (no such folder, not a
//! device the app lists, no device extension). The enabled extensions load as in the app. `OPENFADER_DEVICE_ROOTS` makes a folder
//! a pretend stick, as in the app.

use super::check::{first_cause, line, reason, title, words};
use openfader::device::{
    self, Device,
    check::{self, Fit, Overall, Verdict},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsString};

const USAGE: &str = "Usage: openfader verify <mount> [--players \"MODEL,MODEL\"] [--json]

Checks the USB stick or SD card mounted at <mount>: is it ready for a set, which libraries it holds,
what is wrong with it and which player models read it. Nothing is written to the stick.

  --players LIST  judge it for these player models (default: every model the extensions know)
  --json          print the answer as JSON

Exit code: 0 ready, 1 needs fixing or empty, 2 unreadable, 3 could not be checked.";

/// The command: `args` are those after `verify`; returns the exit code.
pub fn verify(args: Vec<OsString>) -> u8 {
    let mut mount: Option<OsString> = None;
    let (mut booth, mut as_json) = (Vec::new(), false);
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let text = a.to_string_lossy().into_owned();
        match text.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            "--json" => as_json = true,
            "--players" => match args.next() {
                Some(v) => booth = models(&v.to_string_lossy()),
                None => return fail("--players needs a list of player models"),
            },
            t if t.starts_with("--players=") => booth = models(&t["--players=".len()..]),
            t if t.starts_with('-') => return fail(&format!("unknown option {t}\n\n{USAGE}")),
            _ if mount.is_some() => return fail(&format!("one stick at a time\n\n{USAGE}")),
            _ => mount = Some(a),
        }
    }
    let Some(mount) = mount else {
        return fail(&format!(
            "which stick? Give the folder it is mounted at.\n\n{USAGE}"
        ));
    };
    let dev = match stick(&mount) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    let v = check::check(&dev, &BTreeMap::new(), &booth);
    if as_json {
        println!("{:#}", to_json(&dev, &v));
    } else {
        print!("{}", render(&dev, &v));
    }
    v.overall.exit_code()
}

fn fail(msg: &str) -> u8 {
    eprintln!("openfader verify: {msg}");
    3
}

/// "A, B,,C" → [A, B, C].
fn models(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The device the app lists at `mount`, with a plugin that handles it.
fn stick(mount: &OsString) -> Result<Device, String> {
    let want =
        std::fs::canonicalize(mount).map_err(|e| format!("{}: {e}", mount.to_string_lossy()))?;
    if !want.is_dir() {
        return Err(format!("{}: not a folder", want.display()));
    }
    if openfader::plugin::device::providers().is_empty() {
        return Err("no device format is enabled: check Preferences > Library formats".into());
    }
    let dev = device::scan()
        .into_iter()
        .find(|d| std::fs::canonicalize(&d.mount).is_ok_and(|m| m == want))
        .ok_or_else(|| {
            format!(
                "{} is not a USB stick or SD card the app lists (removable media mounted under /run/media or /media, or a drive letter or volume of your OS)",
                want.display()
            )
        })?;
    if dev.provider.is_none() {
        return Err(format!("no enabled extension handles {}", dev.label));
    }
    Ok(dev)
}

fn rule(s: &mut String, head: &str) {
    s.push_str(&format!("\n{head}\n"));
}

/// The text answer.
fn render(dev: &Device, v: &Verdict) -> String {
    let mut s = format!(
        "{} ({})\n{}\n{}\n",
        dev.label,
        dev.summary(),
        title(v.overall),
        line(v)
    );
    rule(&mut s, "LIBRARIES");
    if v.libs.is_empty() {
        s.push_str("  none\n");
    }
    for l in &v.libs {
        let n = |x: Option<usize>| x.map_or_else(|| "-".to_owned(), |n| n.to_string());
        let state = match (l.present, l.readable) {
            (false, _) => "not on this stick".to_owned(),
            (true, false) => "cannot be read".to_owned(),
            (true, true) => l
                .modified
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map_or_else(
                    || "written ?".to_owned(),
                    |t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    },
                ),
        };
        s.push_str(&format!(
            "  {:<18} {state:<18} {:>6} tracks {:>4} playlists {:>6} cues\n",
            l.name,
            n(l.tracks),
            n(l.playlists),
            n(l.cues)
        ));
    }
    if !v.problems.is_empty() {
        rule(&mut s, "WHAT IS WRONG");
    }
    for p in &v.problems {
        let (cause, consequence, action) = words(p);
        let tag = format!("{:?}", p.severity).to_uppercase();
        s.push_str(&format!("  [{tag}] {cause}\n"));
        for t in [consequence, action].iter().filter(|t| !t.is_empty()) {
            s.push_str(&format!("  {:width$} {t}\n", "", width = tag.len() + 2));
        }
    }
    rule(&mut s, "PLAYERS");
    if v.players.is_empty() {
        s.push_str("  no extension lists player models\n");
    }
    for p in &v.players {
        let mark = match p.fit {
            Fit::Ok => "OK ",
            Fit::No(_) => "NO ",
            Fit::Unconfirmed | Fit::Unknown => "?  ",
        };
        let why = reason(p);
        let why = if why.is_empty() {
            why
        } else {
            format!(": {why}")
        };
        s.push_str(&format!("  {mark} {}{why}\n", p.model));
    }
    s
}

/// The JSON answer: the verdict, the libraries, the problems (worded) and the players.
fn to_json(dev: &Device, v: &Verdict) -> Value {
    let verdict = match v.overall {
        Overall::Ready => "ready",
        Overall::NeedsFixing => "needs_fixing",
        Overall::Unreadable => "unreadable",
        Overall::Empty => "empty",
    };
    json!({
        "mount": dev.mount,
        "label": dev.label,
        "file_system": dev.fs,
        "partition_table": dev.table,
        "verdict": verdict,
        "exit_code": v.overall.exit_code(),
        "summary": first_cause(v),
        "libraries": v.libs.iter().map(|l| json!({
            "name": l.name, "present": l.present, "readable": l.readable, "modified": l.modified,
            "tracks": l.tracks, "playlists": l.playlists, "cues": l.cues,
        })).collect::<Vec<_>>(),
        "problems": v.problems.iter().map(|p| {
            let (cause, consequence, action) = words(p);
            json!({
                "id": p.id, "severity": format!("{:?}", p.severity).to_lowercase(), "library": p.library,
                "cause": cause, "consequence": consequence, "action": action, "files": p.files,
            })
        }).collect::<Vec<_>>(),
        "players": v.players.iter().map(|p| json!({
            "model": p.model,
            "fit": match p.fit { Fit::Ok => "ok", Fit::Unconfirmed => "not_confirmed", Fit::No(_) => "no", Fit::Unknown => "unknown" },
            "reason": reason(p),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::device::{Kind, Support};
    use openfader::plugin::device::{DeviceInfo, Player, Problem, Severity};

    fn dev() -> Device {
        Device {
            id: "stick".into(),
            mount: std::env::temp_dir(),
            source: String::new(),
            label: "STICK".into(),
            fs: "vfat".into(),
            table: String::new(),
            kind: Kind::Usb,
            support: Support::Ok,
            read_only: false,
            via_hub: false,
            total: 0,
            free: 0,
            provider: None,
            info: DeviceInfo::default(),
            formats: Vec::new(),
        }
    }

    #[test]
    fn a_list_of_models_is_split_and_trimmed() {
        assert_eq!(
            models(" CDJ-3000X, ,CDJ-2000NXS2 ,"),
            ["CDJ-3000X", "CDJ-2000NXS2"]
        );
        assert!(models("").is_empty());
    }

    #[test]
    fn the_text_and_json_answers_carry_the_verdict_libraries_problems_and_players() {
        let f = check::Facts {
            fs: "vfat".into(),
            libs: vec![check::Lib {
                name: "OneLibrary".into(),
                present: true,
                readable: true,
                tracks: Some(12),
                ..Default::default()
            }],
            problems: vec![Problem {
                id: "p:x".into(),
                severity: Severity::Error,
                cause: "It is incomplete.".into(),
                action: "FIX THE STICK.".into(),
                ..Default::default()
            }],
            players: vec![Player {
                model: "CDJ-3000X".into(),
                reads: vec!["OneLibrary".into()],
                status: "confirmed".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let v = check::judge(&f, &[]);
        let text = render(&dev(), &v);
        for want in [
            "NEEDS FIXING",
            "OneLibrary",
            "12 tracks",
            "[ERROR] It is incomplete.",
            "FIX THE STICK.",
            "OK  CDJ-3000X",
        ] {
            assert!(text.contains(want), "{want} in\n{text}");
        }
        let j = to_json(&dev(), &v);
        assert_eq!(
            (j["verdict"].as_str(), j["exit_code"].as_u64()),
            (Some("needs_fixing"), Some(1))
        );
        assert_eq!(j["libraries"][0]["tracks"], 12);
        assert_eq!(j["problems"][0]["severity"], "error");
        assert_eq!(j["players"][0]["fit"], "ok");
    }
}
