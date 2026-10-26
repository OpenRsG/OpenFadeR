//! The XML writer: a wire [`Snapshot`] as a rekordbox collection (evidence: see the crate docs).

use openfader_plugin::library::Snapshot;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

/// `s` as an XML attribute value: the five entities, newlines/tabs as character references, other
/// control characters (illegal in XML 1.0) dropped.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\n' | '\r' | '\t' => o.push_str(&format!("&#{};", c as u32)),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

/// `/home/user/a b.mp3` → `file://localhost/home/user/a%20b.mp3` (UTF-8 bytes percent-encoded, `/` and `:` kept);
/// a drive path `C:/x` gets the URI's leading slash.
fn uri(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut o = String::from("file://localhost");
    if !p.starts_with('/') {
        o.push('/');
    }
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// rekordbox track colours by ColorID 1..=8 (pink, red, orange, yellow, green, aqua, blue, purple) as the XML `Colour`
/// attribute spells them (inferred from rekordbox's own exports; the Pioneer PDF only says "RGB").
const COLOURS: [u32; 8] = [
    0xFF007F, 0xFF0000, 0xFFA500, 0xFFFF00, 0x00FF00, 0x25FDE9, 0x0000FF, 0x660099,
];

/// Writes the collection of `snap` to the new file `path` (spec: Pioneer xml_format_list.pdf; attribute set as in
/// pyrekordbox rbxml.py @ 43137cb9, see the crate docs); returns the number of tracks. TEMPO: one element per tempo
/// change of the beat grid (left out when `grid` is false), `Metro` = the largest bar position in that segment over 4,
/// `Battito` = bar position of its first beat. POSITION_MARK: the cues.
// ponytail: whole document built in memory; stream it for 100k tracks.
pub fn write(path: &Path, snap: &Snapshot, grid: bool) -> Result<usize, String> {
    let (uniq, pls, members) = (&snap.tracks, &snap.playlists, &snap.members);
    let colors: HashMap<usize, u8> = snap.colors.iter().copied().collect();
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DJ_PLAYLISTS Version=\"1.0.0\">\n",
    );
    let _ = writeln!(
        s,
        "  <PRODUCT Name=\"openfader\" Version=\"{}\" Company=\"\"/>",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(s, "  <COLLECTION Entries=\"{}\">", uniq.len());
    for (i, (t, a)) in uniq.iter().enumerate() {
        let kind = t
            .file
            .as_ref()
            .and_then(|f| f.extension())
            .map(|e| format!("{} File", e.to_string_lossy().to_uppercase()))
            .unwrap_or_default();
        let _ = write!(
            s,
            "    <TRACK TrackID=\"{}\" Name=\"{}\" Artist=\"{}\" Album=\"{}\" Genre=\"{}\" Label=\"{}\" Kind=\"{}\" TotalTime=\"{}\" AverageBpm=\"{}\" Tonality=\"{}\" Rating=\"{}\" Comments=\"{}\" DateAdded=\"{}\" BitRate=\"{}\" SampleRate=\"{}\" Location=\"{}\"",
            i + 1,
            esc(&t.title),
            esc(&t.artist),
            esc(&t.album),
            esc(&t.genre),
            esc(&t.label),
            esc(&kind),
            t.length_s,
            t.bpm_text(),
            esc(&t.key),
            u32::from(t.rating.min(5)) * 51,
            esc(&t.comment),
            esc(&t.date_added),
            t.bitrate,
            t.sample_rate,
            esc(&t.file.as_deref().map(uri).unwrap_or_default()),
        );
        if let Some(&c) = colors.get(&i) {
            let _ = write!(
                s,
                " Colour=\"0x{:06X}\"",
                COLOURS[usize::from(c.clamp(1, 8)) - 1]
            );
        }
        let Some(a) = a
            .as_ref()
            .filter(|a| !a.beats.is_empty() || !a.cues.is_empty())
        else {
            s.push_str("/>\n");
            continue;
        };
        s.push_str(">\n");
        let mut at = 0;
        while grid && at < a.beats.len() {
            let first = a.beats[at];
            let seg = a.beats[at..]
                .iter()
                .take_while(|b| b.bpm_x100 == first.bpm_x100)
                .count();
            let per_bar = a.beats[at..at + seg]
                .iter()
                .map(|b| b.bar_pos)
                .max()
                .unwrap_or(4)
                .max(1);
            let _ = writeln!(
                s,
                "      <TEMPO Inizio=\"{:.3}\" Bpm=\"{:.2}\" Metro=\"{per_bar}/4\" Battito=\"{}\"/>",
                f64::from(first.time_ms) / 1000.0,
                f64::from(first.bpm_x100) / 100.0,
                first.bar_pos.clamp(1, per_bar)
            );
            at += seg;
        }
        for c in &a.cues {
            let _ = write!(
                s,
                "      <POSITION_MARK Name=\"{}\" Type=\"{}\" Start=\"{:.3}\"",
                esc(c.comment.as_deref().unwrap_or("")),
                if c.loop_end_ms.is_some() { 4 } else { 0 },
                f64::from(c.time_ms) / 1000.0
            );
            if let Some(e) = c.loop_end_ms {
                let _ = write!(s, " End=\"{:.3}\"", f64::from(e) / 1000.0);
            }
            let _ = write!(s, " Num=\"{}\"", i32::from(c.hot_cue) - 1);
            if let Some([r, g, b]) = c.color {
                let _ = write!(s, " Red=\"{r}\" Green=\"{g}\" Blue=\"{b}\"");
            }
            s.push_str("/>\n");
        }
        s.push_str("    </TRACK>\n");
    }
    s.push_str("  </COLLECTION>\n  <PLAYLISTS>\n");
    let top = pls.iter().filter(|p| p.depth == 0).count();
    let _ = writeln!(s, "    <NODE Type=\"0\" Name=\"ROOT\" Count=\"{top}\">");
    // Open folders' depths, so a shallower (or equal) row closes them.
    let mut open: Vec<usize> = Vec::new();
    for (n, p) in pls.iter().enumerate() {
        while open.last().is_some_and(|&d| d >= p.depth) {
            let _ = writeln!(s, "{}</NODE>", "  ".repeat(open.len() + 1));
            open.pop();
        }
        let pad = "  ".repeat(open.len() + 2);
        if p.is_folder {
            let kids = pls[n + 1..]
                .iter()
                .take_while(|q| q.depth > p.depth)
                .filter(|q| q.depth == p.depth + 1)
                .count();
            let _ = writeln!(
                s,
                "{pad}<NODE Type=\"0\" Name=\"{}\" Count=\"{kids}\">",
                esc(&p.name)
            );
            open.push(p.depth);
        } else {
            let ids = members.get(n).map_or(&[][..], Vec::as_slice);
            let _ = writeln!(
                s,
                "{pad}<NODE Type=\"1\" Name=\"{}\" KeyType=\"0\" Entries=\"{}\">",
                esc(&p.name),
                ids.len()
            );
            for id in ids {
                let _ = writeln!(s, "{pad}  <TRACK Key=\"{}\"/>", id + 1);
            }
            let _ = writeln!(s, "{pad}</NODE>");
        }
    }
    while open.pop().is_some() {
        let _ = writeln!(s, "{}</NODE>", "  ".repeat(open.len() + 2));
    }
    s.push_str("    </NODE>\n  </PLAYLISTS>\n</DJ_PLAYLISTS>\n");
    let file = compat_rekordbox_common::at(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path),
        path,
    )?;
    let mut out = std::io::BufWriter::new(file);
    compat_rekordbox_common::at(out.write_all(s.as_bytes()).and_then(|_| out.flush()), path)?;
    Ok(uniq.len())
}
