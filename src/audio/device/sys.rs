//! What the machine says about its sound cards, shared by [`super::card`] (titles, HDMI state) and [`super::volume`]
//! (the sound server's sink). Sources, in the order they are preferred:
//!
//! * `pactl -f json list sinks` (PipeWire / PulseAudio) [observed on this machine: PipeWire 1.x via pipewire-pulse]:
//!   `description` ("A50 Analog Stereo"), `mute`, `volume.<channel>.{value_percent,db}`, `properties."alsa.id"` (the ALSA
//!   card id the sink belongs to). `pactl get-default-sink` names the sink the sound-server PCMs play on.
//! * `/proc/asound/cards`: `N [id            ]: driver - short name`.
//! * `/run/udev/data/+sound:cardN`: `ID_MODEL_FROM_DATABASE` (PCI cards: "GA102 High Definition Audio Controller"),
//!   `ID_VENDOR` + `ID_MODEL` (USB cards: "Logitech A50").
//! * HDMI / DisplayPort connection: the HDA jack control `HDMI/DP,pcm=<d> Jack` (measured on one NVIDIA card: hda_jack names the element after
//!   the PCM device number; `hdmi:CARD=…,DEV=n` is the PCM whose `/proc/asound/cardN/pcm<d>p/info` says `id: HDMI n`; the
//!   monitor sits on codec pin 0x5 while PCM 3 (DEV=0) is the first HDMI PCM, so the ELD files cannot be
//!   matched to a DEV by position, the jack control carries the PCM number itself).
//!
//! Everything is read with a plain process / file read; a missing tool or file just leaves its facts out. All of it is Linux's:
//! on Windows and macOS [`facts`] is empty (cpal names each device plainly there) and the parsers below are unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use parking_lot::Mutex;
use std::collections::HashMap;
use std::process::Command;
use std::time::Instant;

/// One sink as `pactl` reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sink {
    pub name: String,
    pub description: String,
    /// The ALSA card id (`alsa.id`) the sink plays on; `None` for a Bluetooth or virtual sink.
    pub alsa_id: Option<String>,
    /// First channel's volume in PipeWire percent (the cube root of the amplitude) and in dB.
    pub percent: Option<u32>,
    pub db: Option<f32>,
    pub muted: bool,
}

/// One line pair of `/proc/asound/cards`, plus the udev model name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysCard {
    pub num: u32,
    pub id: String,
    pub driver: String,
    pub name: String,
    pub model: Option<String>,
    /// The card has a playback PCM (`/proc/asound/cardN/pcm<d>p`).
    pub playback: bool,
}

/// Everything [`super::card::enrich`] needs.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub sinks: Vec<Sink>,
    /// Name of the sink the sound-server PCMs play on.
    pub default_sink: Option<String>,
    pub cards: Vec<SysCard>,
    /// `(card id, HDMI DEV)` → a monitor is connected.
    pub plugged: HashMap<(String, u32), bool>,
}

impl Facts {
    /// The sink of the ALSA card `id`.
    pub fn sink_of(&self, id: &str) -> Option<&Sink> {
        self.sinks.iter().find(|s| s.alsa_id.as_deref() == Some(id))
    }
}

/// Runs `pactl args…` (C locale) and returns its stdout; `None` when it is missing, fails or the server is down.
pub fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The sinks in `pactl -f json list sinks` output.
pub fn parse_sinks(json: &str) -> Vec<Sink> {
    let Ok(serde_json::Value::Array(all)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    all.iter()
        .filter_map(|s| {
            let ch = s["volume"].as_object().and_then(|m| m.values().next());
            Some(Sink {
                name: s["name"].as_str()?.to_owned(),
                description: s["description"].as_str().unwrap_or_default().to_owned(),
                alsa_id: s["properties"]["alsa.id"].as_str().map(str::to_owned),
                percent: ch
                    .and_then(|c| c["value_percent"].as_str())
                    .and_then(|p| p.trim_end_matches('%').parse().ok()),
                db: ch
                    .and_then(|c| c["db"].as_str())
                    .and_then(|d| d.trim_end_matches(" dB").parse().ok()),
                muted: s["mute"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// The current sinks (not cached: volume reads want the live value).
pub fn sinks() -> Vec<Sink> {
    pactl(&["-f", "json", "list", "sinks"]).map_or_else(Vec::new, |j| parse_sinks(&j))
}

/// The sink the sound-server PCMs play on.
pub fn default_sink() -> Option<String> {
    pactl(&["get-default-sink"])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// The cards listed in `/proc/asound/cards`.
pub fn parse_cards(text: &str) -> Vec<SysCard> {
    text.lines()
        .filter_map(|l| {
            let (num, rest) = l.trim_start().split_once(" [")?;
            let (id, rest) = rest.split_once(']')?;
            let (driver, name) = rest.trim_start_matches(':').trim().split_once(" - ")?;
            Some(SysCard {
                num: num.trim().parse().ok()?,
                id: id.trim().to_owned(),
                driver: driver.to_owned(),
                name: name.trim().to_owned(),
                model: None,
                playback: false,
            })
        })
        .collect()
}

/// A card's human model from its udev record: the hardware database name, else "vendor model".
pub fn udev_model(text: &str) -> Option<String> {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix("E:")?.strip_prefix(k)?.strip_prefix('='))
            .map(|v| v.replace('_', " "))
            .filter(|v| !v.is_empty())
    };
    get("ID_MODEL_FROM_DATABASE").or_else(|| match (get("ID_VENDOR"), get("ID_MODEL")) {
        (Some(v), Some(m)) if !m.starts_with(&v) => Some(format!("{v} {m}")),
        (_, m) => m,
    })
}

/// `n` of an HDMI PCM's `info` file (`id: HDMI n`).
pub fn hdmi_index(info: &str) -> Option<u32> {
    info.lines()
        .find_map(|l| l.strip_prefix("id: HDMI "))?
        .trim()
        .parse()
        .ok()
}

/// Connection state of every HDMI / DisplayPort PCM of card `num`: `(DEV n, connected)`.
#[cfg(target_os = "linux")]
fn hdmi_plugged(num: u32) -> Vec<(u32, bool)> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/asound/card{num}")) else {
        return Vec::new();
    };
    let Ok(ctl) = alsa::Ctl::new(&format!("hw:{num}"), false) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let pcm: u32 = e
                .file_name()
                .to_str()?
                .strip_prefix("pcm")?
                .strip_suffix('p')?
                .parse()
                .ok()?;
            let n = hdmi_index(&std::fs::read_to_string(e.path().join("info")).ok()?)?;
            let mut id = alsa::ctl::ElemId::new(alsa::ctl::ElemIface::Card);
            id.set_name(&std::ffi::CString::new(format!("HDMI/DP,pcm={pcm} Jack")).ok()?);
            let mut v = alsa::ctl::ElemValue::new(alsa::ctl::ElemType::Boolean).ok()?;
            v.set_id(&id);
            ctl.elem_read(&mut v).ok()?;
            Some((n, v.get_boolean(0)?))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn read_facts() -> Facts {
    Facts::default()
}

#[cfg(target_os = "linux")]
fn read_facts() -> Facts {
    let mut cards = std::fs::read_to_string("/proc/asound/cards")
        .map_or_else(|_| Vec::new(), |t| parse_cards(&t));
    let mut plugged = HashMap::new();
    for c in &mut cards {
        c.model = std::fs::read_to_string(format!("/run/udev/data/+sound:card{}", c.num))
            .ok()
            .and_then(|t| udev_model(&t));
        c.playback = std::fs::read_dir(format!("/proc/asound/card{}", c.num)).is_ok_and(|d| {
            d.flatten().any(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("pcm") && n.ends_with('p')
            })
        });
        for (n, on) in hdmi_plugged(c.num) {
            plugged.insert((c.id.clone(), n), on);
        }
    }
    Facts {
        sinks: sinks(),
        default_sink: default_sink(),
        cards,
        plugged,
    }
}

/// Shared by every thread (see [`super::CACHE`]).
static CACHE: Mutex<Option<(Instant, Facts)>> = Mutex::new(None);

/// The facts, reused for [`super::TTL`].
pub fn facts() -> Facts {
    if let Some((_, f)) = CACHE.lock().as_ref().filter(|(t, _)| super::usable(t)) {
        return f.clone();
    }
    if super::UI.get() {
        return Facts::default();
    }
    let f = read_facts();
    *CACHE.lock() = Some((Instant::now(), f.clone()));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pactl_sinks_give_the_description_volume_and_card() {
        // Captured from `pactl -f json list sinks` on this machine (trimmed).
        let json = r#"[{"name":"alsa_output.usb-Logitech_A50-00.analog-stereo","description":"A50 Analog Stereo","mute":false,
            "volume":{"front-left":{"value":13108,"value_percent":"20%","db":"-41.94 dB"},
                      "front-right":{"value":13108,"value_percent":"20%","db":"-41.94 dB"}},
            "properties":{"alsa.id":"A50","alsa.card":"0"}},
            {"name":"bt","description":"Headset","mute":true,"volume":{},"properties":{}}]"#;
        let s = parse_sinks(json);
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].description.as_str(), s[0].percent, s[0].db, s[0].muted),
            ("A50 Analog Stereo", Some(20), Some(-41.94), false)
        );
        assert_eq!(s[0].alsa_id.as_deref(), Some("A50"));
        assert_eq!(
            (s[1].alsa_id.clone(), s[1].percent, s[1].muted),
            (None, None, true)
        );
        assert!(parse_sinks("not json").is_empty());
    }

    #[test]
    fn proc_cards_and_udev_records_name_the_hardware() {
        let cards = parse_cards(
            " 0 [A50            ]: USB-Audio - A50\n                      Logitech A50 at usb-0000:0e:00.0-5.1, full speed\n \
             1 [NVidia         ]: HDA-Intel - HDA NVidia\n                      HDA NVidia at 0xf6080000 irq 119\n \
             3 [Generic_1      ]: HDA-Intel - HD-Audio Generic\n                      HD-Audio Generic at 0xf6780000 irq 123\n",
        );
        assert_eq!(
            cards
                .iter()
                .map(|c| (c.num, c.id.as_str(), c.name.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, "A50", "A50"),
                (1, "NVidia", "HDA NVidia"),
                (3, "Generic_1", "HD-Audio Generic")
            ]
        );
        assert_eq!(
            udev_model("E:ID_VENDOR_FROM_DATABASE=NVIDIA Corporation\nE:ID_MODEL_FROM_DATABASE=GA102 High Definition Audio Controller\n").as_deref(),
            Some("GA102 High Definition Audio Controller")
        );
        assert_eq!(
            udev_model("E:ID_VENDOR=Logitech\nE:ID_MODEL=A50\n").as_deref(),
            Some("Logitech A50")
        );
        assert_eq!(udev_model("E:ID_BUS=pci\n"), None);
    }

    #[test]
    fn an_hdmi_pcm_info_gives_its_dev_index() {
        assert_eq!(
            hdmi_index("card: 1\ndevice: 7\nid: HDMI 1\nname: HDMI 1\n"),
            Some(1)
        );
        assert_eq!(hdmi_index("id: ALC892 Analog\n"), None);
    }
}
