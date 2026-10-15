//! The user's own bindings, `$XDG_CONFIG_HOME/openfader/midi.map` (`~/.config/…`): one
//! `<status> <data1> -> <action> [deck]` per line, `#` comments, numbers decimal or `0x`hex, deck
//! `a`/`b` (default a). They override the controller mapping file's rows for the same control.

use super::Binding;
use super::actions::ACTIONS;
use openfader::paths::config_dir;
use std::path::PathBuf;

fn num(s: &str) -> Result<u8, String> {
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u8::from_str_radix(h, 16),
        None => s.parse(),
    };
    n.map_err(|_| format!("bad number {s:?}"))
}

/// `Ok(None)` for blank and comment lines.
fn parse_line(line: &str) -> Result<Option<Binding>, String> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let (l, r) = line.split_once("->").ok_or("missing ->")?;
    let (l, r): (Vec<_>, Vec<_>) = (
        l.split_whitespace().collect(),
        r.split_whitespace().collect(),
    );
    let ([s, d1], [name, rest @ ..]) = (l.as_slice(), r.as_slice()) else {
        return Err("want `<status> <data1> -> <action> [deck]`".into());
    };
    let (status, d1) = (num(s)?, num(d1)?);
    if !matches!(status & 0xF0, 0x90 | 0xB0) || d1 > 127 {
        return Err("status must be note-on (0x9n) or CC (0xBn), data1 0..127".into());
    }
    let act = ACTIONS
        .iter()
        .position(|a| a.name == *name && a.learnable())
        .ok_or_else(|| format!("unknown action {name:?}"))?;
    let deck = match rest {
        [] | ["a" | "A" | "0"] => 0,
        ["b" | "B" | "1"] => 1,
        _ => return Err("deck must be a or b".into()),
    };
    Ok(Some(Binding {
        status,
        d1,
        act,
        deck,
        arg: 0,
        hires: false,
    }))
}

fn format_line(b: &Binding) -> String {
    format!(
        "0x{:02X} 0x{:02X} -> {} {}",
        b.status,
        b.d1,
        ACTIONS[b.act].name,
        ["a", "b"][b.deck as usize]
    )
}

/// Every parsable binding; bad lines are reported and skipped. A later line for the same control wins.
fn parse_map(text: &str) -> Vec<Binding> {
    let mut v: Vec<Binding> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        match parse_line(line) {
            Ok(Some(b)) => {
                v.retain(|o| (o.status, o.d1) != (b.status, b.d1));
                v.push(b);
            }
            Ok(None) => {}
            Err(e) => eprintln!("midi.map line {}: {e}", n + 1),
        }
    }
    v
}

/// `midi.map` inside the config folder.
fn map_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("midi.map"))
}

pub fn load() -> Vec<Binding> {
    map_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_map(&t))
        .unwrap_or_default()
}

/// Writes `bindings` as the whole `midi.map`.
pub fn save(bindings: &[Binding]) -> Result<(), String> {
    let text: String = bindings.iter().map(|b| format_line(b) + "\n").collect();
    let path = map_path().ok_or("no config dir")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_file_parses_and_round_trips() {
        let text =
            "# comment\n0x90 0x0B -> play\n0xB0 7 -> fader b  # trailing\n\n176 8 -> tempo A\n";
        let v = parse_map(text);
        assert_eq!(v.len(), 3);
        assert_eq!(
            (v[0].status, v[0].d1, ACTIONS[v[0].act].name, v[0].deck),
            (0x90, 11, "play", 0)
        );
        assert_eq!((ACTIONS[v[1].act].name, v[1].deck), ("fader", 1));
        assert_eq!((v[2].status, v[2].d1), (0xB0, 8));
        let again = parse_map(&v.iter().map(|b| format_line(b) + "\n").collect::<String>());
        assert_eq!(again, v);
    }

    #[test]
    fn bad_lines_are_rejected_and_later_line_wins() {
        for bad in [
            "0x90 1 play",
            "0x90 1 -> nope",
            "0x80 1 -> play",
            "0x90 200 -> play",
            "0x90 1 -> play c",
            "zz 1 -> play",
            "0x90 1 -> hw-pad",
        ] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
        let v = parse_map("0x90 1 -> play\n0x90 1 -> cue b\n");
        assert_eq!(v.len(), 1);
        assert_eq!(ACTIONS[v[0].act].name, "cue");
    }
}
