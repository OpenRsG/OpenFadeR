//! Our controller mapping files (`<controller>.xml`, the OpenFadeR-Map repository's files) in the mapping folders: each folder of
//! `$OPENFADER_MAPPINGS`, `<config>/openfader/mappings`, then `mappings/` next to the executable; a file in an earlier folder
//! replaces one of the same name in a later one. One file per controller: a
//! port-name match, a list of input controls (MIDI status + number → one of our [`ACTIONS`] with its deck and constant) and
//! a list of outputs (one of our lights → MIDI status + number):
//!
//! ```xml
//! <OpenFadeRMapping schemaVersion="1">
//!   <info>
//!     <name>Example Controller</name>
//!     <port>Example</port>           <!-- picked when an input port name contains it (longest wins) -->
//!     <outInterval>2</outInterval>   <!-- ms between two messages to the controller (optional) -->
//!     <ringInterval>12</ringInterval> <!-- jog ring refresh in ms (optional) -->
//!   </info>
//!   <controls>
//!     <control status="0x90" midino="0x0B" action="play" deck="0"/>
//!     <control status="0xB4" midino="0x02" action="fx-level-1" deck="0" hires="true"/> <!-- 14-bit: LSB on midino + 0x20 -->
//!     <control status="0x97" midino="0x00" action="hw-pad" deck="0" arg="0"/>
//!   </controls>
//!   <outputs>
//!     <output status="0x90" midino="0x58" light="Sync" deck="0" blink="600"/>
//!     <output status="0xB4" midino="0x64" light="FxBeat(0)"/>
//!   </outputs>
//! </OpenFadeRMapping>
//! ```
//!
//! `deck` is 0..3 (deck A..D; the FX unit or MERGE side for those actions), `arg` the action's constant (default 0),
//! `light` one of [`Led`] written as in Rust.

use super::{Binding, Led, LedRow, Msg, actions::ACTIONS};
use openfader::library::markup::{Edge, tags};
use openfader::paths::search_path;
use std::collections::HashMap;
#[cfg(test)]
use std::fmt::Write as _;

/// What a mapping file holds.
#[derive(Default, Debug)]
pub(super) struct Mapping {
    pub controller: String,
    pub binds: Vec<Binding>,
    /// The action name of every input message in the file, for the log.
    pub names: HashMap<Msg, String>,
    pub leds: Vec<LedRow>,
    /// `<outInterval>`: the gap between two messages to the controller, ms. `None` = not given.
    pub out_ms: Option<u64>,
    /// `<ringInterval>`: how often the jog ring is refreshed, ms. `None` = not given.
    pub ring_ms: Option<u64>,
    /// `<stateRequest>`: the system-exclusive message that makes the controller report its state (empty = none).
    pub request: Vec<u8>,
    /// Entries the parser had to skip.
    pub bad: Vec<String>,
}

/// The file text of `m`; the round trip of every built-in file is tested.
#[cfg(test)]
fn write(m: &Mapping, port: &str) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<OpenFadeRMapping schemaVersion=\"1\">\n  <info>\n",
    );
    let _ = writeln!(s, "    <name>{}</name>", esc(&m.controller));
    let _ = writeln!(s, "    <port>{}</port>", esc(port));
    if let Some(ms) = m.out_ms {
        let _ = writeln!(s, "    <outInterval>{ms}</outInterval>");
    }
    if let Some(ms) = m.ring_ms {
        let _ = writeln!(s, "    <ringInterval>{ms}</ringInterval>");
    }
    if !m.request.is_empty() {
        let hex: Vec<_> = m.request.iter().map(|b| format!("{b:02X}")).collect();
        let _ = writeln!(s, "    <stateRequest>{}</stateRequest>", hex.join(" "));
    }
    s += "  </info>\n  <controls>\n";
    for b in &m.binds {
        let _ = write!(
            s,
            "    <control status=\"0x{:02X}\" midino=\"0x{:02X}\" action=\"{}\" deck=\"{}\"",
            b.status, b.d1, ACTIONS[b.act].name, b.deck
        );
        if b.arg != 0 {
            let _ = write!(s, " arg=\"{}\"", b.arg);
        }
        if b.hires {
            s += " hires=\"true\"";
        }
        s += "/>\n";
    }
    s += "  </controls>\n  <outputs>\n";
    for l in &m.leds {
        let _ = write!(
            s,
            "    <output status=\"0x{:02X}\" midino=\"0x{:02X}\" light=\"{}\" deck=\"{}\"",
            l.msg.0,
            l.msg.1,
            format!("{:?}", l.led).replace(' ', ""),
            l.deck
        );
        if let Some(ms) = l.blink {
            let _ = write!(s, " blink=\"{ms}\"");
        }
        s += "/>\n";
    }
    s += "  </outputs>\n</OpenFadeRMapping>\n";
    s
}

#[cfg(test)]
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A mapping file: the mapping and its port-name match.
pub(super) fn parse(text: &str) -> Result<(Mapping, String), String> {
    let mut m = Mapping::default();
    let mut port = String::new();
    for t in tags(text)? {
        let num = |k: &str| -> Result<i64, String> {
            let v = t
                .attrs
                .get(k)
                .ok_or_else(|| format!("<{}> without {k}", t.name))?;
            let r = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                Some(h) => i64::from_str_radix(h, 16),
                None => v.parse(),
            };
            r.map_err(|_| tr!("<{}> {}=\"{}\" is not a number", t.name, k, v))
        };
        let opt = |k: &str| t.attrs.contains_key(k).then(|| num(k)).transpose();
        let byte = |k: &str| {
            num(k).and_then(|n| u8::try_from(n).map_err(|_| tr!("{}={} is not a byte", k, n)))
        };
        match (t.name.as_str(), t.edge) {
            ("name" | "port" | "outInterval" | "ringInterval" | "stateRequest", Edge::Open) => {
                let v = t.text.trim().to_owned();
                match t.name.as_str() {
                    "name" => m.controller = v,
                    "port" => port = v,
                    "outInterval" => m.out_ms = v.parse().ok(),
                    "ringInterval" => m.ring_ms = v.parse().ok(),
                    _ => match v
                        .split_whitespace()
                        .map(|b| u8::from_str_radix(b, 16))
                        .collect()
                    {
                        Ok(bytes) => m.request = bytes,
                        Err(_) => m.bad.push(tr!("<stateRequest> \"{}\" is not hex bytes", v)),
                    },
                }
            }
            ("control", Edge::Empty | Edge::Open) => {
                let name = t.attrs.get("action").map(String::as_str).unwrap_or("");
                let Some(act) = ACTIONS.iter().position(|a| a.name == name) else {
                    m.bad.push(tr!("unknown action \"{}\"", name));
                    continue;
                };
                let b = Binding {
                    status: byte("status")?,
                    d1: byte("midino")?,
                    act,
                    deck: opt("deck")?.unwrap_or(0) as i32,
                    arg: opt("arg")?.unwrap_or(0) as i32,
                    hires: t.attrs.get("hires").is_some_and(|v| v == "true"),
                };
                m.names.insert((b.status, b.d1), name.to_owned());
                m.binds.push(b);
            }
            ("output", Edge::Empty | Edge::Open) => {
                let light = t.attrs.get("light").map(String::as_str).unwrap_or("");
                let Some(led) = led(light) else {
                    m.bad.push(tr!("unknown light \"{}\"", light));
                    continue;
                };
                m.leds.push(LedRow {
                    led,
                    deck: opt("deck")?.unwrap_or(0) as usize,
                    msg: (byte("status")?, byte("midino")?),
                    blink: opt("blink")?.map(|n| n as u64).filter(|&n| n > 0),
                });
            }
            _ => {}
        }
    }
    if m.controller.is_empty() {
        return Err(tr!("no <name>").into());
    }
    if port.is_empty() {
        port = m.controller.clone();
    }
    Ok((m, port))
}

/// A light as [`write`] spells it: the [`Led`] variant and its fields, `Fx(0,2)`.
fn led(s: &str) -> Option<Led> {
    let (name, args): (&str, Vec<&str>) = match s.split_once('(') {
        Some((n, r)) => (n, r.strip_suffix(')')?.split(',').map(str::trim).collect()),
        None => (s, vec![]),
    };
    let u = |i: usize| args.get(i)?.parse::<usize>().ok();
    let n = |i: usize| args.get(i)?.parse::<i32>().ok();
    let b = |i: usize| args.get(i)?.parse::<bool>().ok();
    let l = match name {
        "Play" => Led::Play,
        "Cue" => Led::Cue,
        "Sync" => Led::Sync,
        "Master" => Led::Master,
        "Loop" => Led::Loop,
        "Cutter" => Led::Cutter,
        "Vinyl" => Led::Vinyl,
        "HpCue" => Led::HpCue,
        "Loaded" => Led::Loaded,
        "Sampler" => Led::Sampler,
        "TempoRange" => Led::TempoRange,
        "TrackStart" => Led::TrackStart,
        "Tap" => Led::Tap,
        "Loop4" => Led::Loop4,
        "Level" => Led::Level,
        "Ring" => Led::Ring,
        "Iso" => Led::Iso,
        "DrumCapture" => Led::DrumCapture,
        "Mode" => Led::Mode(n(0)?),
        "Pad" => Led::Pad(n(0)?, u(1)?),
        "PadSlot" => Led::PadSlot(u(0).filter(|&s| s < 8)?),
        "Fx" => Led::Fx(u(0)?, u(1)?),
        "FxEffect" => Led::FxEffect(u(0)?, u(1)?, n(2)?, b(3)?),
        "FxTapped" => Led::FxTapped(u(0)?, b(1)?),
        "FxAssign" => Led::FxAssign(u(0)?, n(1)?),
        "FxChOn" => Led::FxChOn(u(0)?, n(1)?, u(2)?),
        "FxRelease" => Led::FxRelease(u(0)?),
        "FxBeat" => Led::FxBeat(u(0)?),
        "SamplerPad" => Led::SamplerPad(u(0)?),
        "MergePreset" => Led::MergePreset(u(0)?, u(1)?),
        "MergeOn" => Led::MergeOn(u(0)?),
        "Stem" => Led::Stem(u(0)?),
        "FxPart" => Led::FxPart(n(0)?),
        "Drum" => Led::Drum(u(0)?),
        "DrumBank" => Led::DrumBank(u(0)?),
        "DrumMode" => Led::DrumMode(b(0)?),
        _ => return None,
    };
    // Extra or malformed fields: only the exact spelling counts.
    (format!("{l:?}").replace(' ', "") == s.replace(' ', "")).then_some(l)
}

/// Every mapping file of the mapping folders, by name; the earlier folder's file wins.
pub(super) fn files() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for dir in search_path("OPENFADER_MAPPINGS", "mappings")
        .into_iter()
        .rev()
    {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".xml") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                out.retain(|(n, _)| *n != name);
                out.push((name, text));
            }
        }
    }
    out
}

/// The mapping for the connected input `ports`: the file whose `<port>` is the longest one found inside a port name
/// (case-insensitive). `Ok(None)` when none matches (the user's midi.map still works), `Err` for a broken file.
pub(super) fn load(ports: &[String]) -> Result<Option<Mapping>, String> {
    pick(ports, files())
}

fn pick(ports: &[String], files: Vec<(String, String)>) -> Result<Option<Mapping>, String> {
    let mut best: Option<(usize, Mapping)> = None;
    for (name, text) in files {
        let (m, port) = parse(&text).map_err(|e| format!("{name}: {e}"))?;
        if let Some(len) = match_len(ports, &port)
            && best.as_ref().is_none_or(|(l, _)| len > *l)
        {
            best = Some((len, m));
        }
    }
    Ok(best.map(|(_, m)| m))
}

/// How well a file's `<port>` fits the connected input `ports`: its length when one port name contains it (case-insensitive), `None`
/// when none does. The longest match wins ([`pick`]).
pub(super) fn match_len(ports: &[String], port: &str) -> Option<usize> {
    let port = port.to_lowercase();
    ports
        .iter()
        .any(|p| p.to_lowercase().contains(&port))
        .then_some(port.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, port: &str) -> (String, String) {
        (
            name.into(),
            format!(
                "<OpenFadeRMapping><info><name>{name}</name><port>{port}</port></info><controls>\
                 <control status=\"0x90\" midino=\"0x0B\" action=\"play\" deck=\"1\"/>\
                 <control status=\"0xB5\" midino=\"0x02\" action=\"fx-level-1\" deck=\"1\" hires=\"true\"/>\
                 </controls><outputs>\
                 <output status=\"0xB4\" midino=\"0x64\" light=\"FxBeat(0)\"/>\
                 </outputs></OpenFadeRMapping>"
            ),
        )
    }

    #[test]
    fn every_checked_out_mapping_reads_back_to_the_same_file() {
        // The OpenFadeR-Map checkout in `mappings/` (start.sh fetches it); nothing to check without it.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings");
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".xml") {
                continue;
            }
            let text = std::fs::read_to_string(e.path()).unwrap();
            let (m, port) = parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(m.bad.is_empty(), "{name}: {:?}", m.bad);
            assert_eq!(write(&m, &port), text, "{name}");
        }
    }

    #[test]
    fn a_file_carries_its_controls_and_lights() {
        let m = pick(
            &["Test Controller:Test Controller MIDI 1 20:0".into()],
            vec![file("Test Controller", "Test Controller")],
        )
        .unwrap()
        .unwrap();
        assert_eq!(m.controller, "Test Controller");
        let bind = |s, d| {
            m.binds
                .iter()
                .find(|b| (b.status, b.d1) == (s, d))
                .map(|b| (ACTIONS[b.act].name, b.deck, b.arg, b.hires))
        };
        assert_eq!(bind(0x90, 0x0B), Some(("play", 1, 0, false)));
        assert_eq!(bind(0xB5, 0x02), Some(("fx-level-1", 1, 0, true)));
        assert!(
            m.leds
                .iter()
                .any(|l| l.led == Led::FxBeat(0) && l.msg == (0xB4, 0x64))
        );
    }

    #[test]
    fn the_longest_port_match_wins_and_unknown_names_are_reported() {
        let files = || vec![file("Test", "Test"), file("Test Pro", "Test Pro")];
        assert!(
            pick(&["Midi Through:Midi Through Port-0 14:0".into()], files())
                .unwrap()
                .is_none()
        );
        let pro = pick(&["Test Pro MIDI 1".into()], files()).unwrap().unwrap();
        assert_eq!(pro.controller, "Test Pro", "not the shorter Test");
        let (m, port) = parse(
            "<OpenFadeRMapping><info><name>X</name></info><controls>\
             <control status=\"0x90\" midino=\"11\" action=\"nope\"/></controls><outputs>\
             <output status=\"0x90\" midino=\"0x0B\" light=\"Fx(0,9,9)\"/></outputs></OpenFadeRMapping>",
        )
        .unwrap();
        assert_eq!(port, "X", "no <port>: the name");
        assert_eq!(
            m.bad,
            ["unknown action \"nope\"", "unknown light \"Fx(0,9,9)\""]
        );
    }
}
