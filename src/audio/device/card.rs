//! The OS output (or input) devices as a person thinks of them: one [`Card`] per sound card with named [`Port`]s,
//! each port mapped to the exact cpal name the engine opens (`audio::device::output_named`).
//!
//! * cpal's ALSA host lists the PCM names of `snd_device_name_hint` ("alsa.conf"): `<plugin>:CARD=<id>[,DEV=<n>]`
//!   with the plugins `sysdefault` (the card through the `plug` converter), `front` (2 channels), `surround21/40/41/50/51/71`,
//!   `iec958` (S/PDIF), `hdmi` (DEV = HDMI index), `hw`, `plughw`, `dmix`, `dsnoop`, `usbstream` (raw access / mixing), plus
//!   card-less plugins (`lavrate`, `samplerate`, `speexrate`, `speex`, `upmix`, `vdownmix`, `jack`, `oss`, `a52`, `null`) and
//!   the sound-server PCMs (`pipewire`, `pulse`, `default`).
//! * [observed] the names on this machine: sysdefault, lavrate, samplerate, speexrate, jack, pipewire, pulse, speex, upmix,
//!   vdownmix, default, `sysdefault:CARD=A50`, `front:CARD=A50,DEV=0`, `surround40:CARD=A50,DEV=0`, `iec958:CARD=…`,
//!   `hdmi:CARD=NVidia,DEV=0..3`, `surround21..71:CARD=Generic_1,DEV=0`, `sysdefault:CARD=<controller>` (see the tests).
//! * [inferred] port wording follows PipeWire's ("Analog Output", "HDMI / DisplayPort", "HDMI / DisplayPort 2" for DEV=1:
//!   PipeWire counts HDMI outputs from 1) so the list reads like the user's own sound manager.
//!
//! Plugins and converters are hidden unless `show_all`; nothing is lost, [`group`] then lists them as extra ports.
//! `saved` (the name stored in the settings) always stays selectable, so an old setting keeps its card.

use super::Facts;

/// What a [`Card`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The system default device (the stored name is empty).
    Default,
    /// The sound server (`pipewire`, `pulse`, `default`).
    Server,
    /// One physical ALSA card.
    Card,
    /// Names that belong to no card (plugins, converters), shown with "show all".
    Raw,
}

/// One way to open a card.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    /// "Analog Output", "HDMI / DisplayPort 2", or the raw name for a hidden plugin.
    pub label: String,
    /// The exact cpal name ("" = the system default).
    pub name: String,
    /// Channels the name promises (`surround40` = 4); 0 = not in the name.
    pub channels: usize,
    /// DEV of an HDMI port.
    pub hdmi: Option<u32>,
    /// Monitor connected, when the system says.
    pub plugged: Option<bool>,
}

impl Port {
    /// A port with only a label and a cpal name.
    fn plain(label: &str, name: &str) -> Port {
        Port {
            label: label.to_owned(),
            name: name.to_owned(),
            channels: 0,
            hdmi: None,
            plugged: None,
        }
    }

    /// The label as shown in a list: "HDMI / DisplayPort (plugged in)".
    pub fn text(&self) -> String {
        match self.plugged {
            Some(true) => format!("{} (plugged in)", self.label),
            Some(false) => format!("{} (unplugged)", self.label),
            None => self.label.clone(),
        }
    }
}

/// One entry of the device list: the system default, the sound server, a sound card, or the raw plugins.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub kind: Kind,
    /// The ALSA card id (`A50`, `Generic_1`); empty for the other kinds.
    pub id: String,
    pub title: String,
    /// A second line: what the default plays on, the driver.
    pub note: String,
    pub ports: Vec<Port>,
    /// The saved device is not in the OS list.
    pub missing: bool,
    /// The card has a playback PCM but cpal cannot open it: the sound server is playing on it.
    pub busy: bool,
}

impl Card {
    /// A stable handle for the UI (survives list changes): `default`, `server`, `raw` or `card:<id>`.
    pub fn key(&self) -> String {
        match self.kind {
            Kind::Default => "default".to_owned(),
            Kind::Server => "server".to_owned(),
            Kind::Raw => "raw".to_owned(),
            Kind::Card => format!("card:{}", self.id),
        }
    }

    /// The port a click on the card opens: the first with a monitor connected, else the first.
    pub fn default_port(&self) -> usize {
        self.ports
            .iter()
            .position(|p| p.plugged == Some(true))
            .unwrap_or(0)
    }

    /// The port that opens the cpal name `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.ports.iter().position(|p| p.name == name)
    }
}

/// A cpal name taken apart.
#[derive(Debug, PartialEq)]
struct Parsed<'a> {
    plugin: &'a str,
    card: Option<&'a str>,
    dev: Option<u32>,
}

/// `plugin:CARD=id,DEV=n`, or the positional `plugin:id,n` of `hw:0,0`. Only ALSA names have that shape: the WASAPI and CoreAudio
/// hosts name a device by plain text ("Speakers (Realtek Audio)"), which belongs to no card.
fn parse(name: &str) -> Parsed<'_> {
    if !ALSA {
        return Parsed {
            plugin: name,
            card: None,
            dev: None,
        };
    }
    let (plugin, args) = name.split_once(':').unwrap_or((name, ""));
    let (mut card, mut dev) = (None, None);
    if args.contains('=') {
        for kv in args.split(',') {
            match kv.split_once('=') {
                Some(("CARD", v)) => card = Some(v),
                Some(("DEV", v)) => dev = v.parse().ok(),
                _ => {}
            }
        }
    } else if !args.is_empty() {
        let mut it = args.split(',');
        card = it.next();
        dev = it.next().and_then(|d| d.parse().ok());
    }
    Parsed { plugin, card, dev }
}

/// `(card id, DEV)` of an HDMI / DisplayPort cpal name (`hdmi:CARD=Generic,DEV=0`); `None` for any other name.
pub(super) fn hdmi_port(name: &str) -> Option<(String, u32)> {
    let p = parse(name);
    if p.plugin != "hdmi" {
        return None;
    }
    Some((p.card?.to_owned(), p.dev.unwrap_or(0)))
}

/// The visible port of a known plugin: `(sort rank, port)`; `None` for everything that stays hidden.
fn known(name: &str, p: &Parsed) -> Option<(u32, Port)> {
    let dev = p.dev.unwrap_or(0);
    let many = if dev > 0 {
        format!(" {}", dev + 1)
    } else {
        String::new()
    };
    let (rank, label, channels, hdmi) = match p.plugin {
        "front" => (0, format!("Analog Output{many}"), 2, None),
        "sysdefault" => (1, format!("Default{many}"), 0, None),
        "iec958" => (90, format!("Digital Output (S/PDIF){many}"), 2, None),
        "hdmi" => (
            100 + dev,
            if dev == 0 {
                "HDMI / DisplayPort".to_owned()
            } else {
                format!("HDMI / DisplayPort {}", dev + 1)
            },
            2,
            Some(dev),
        ),
        s => {
            let d = s.strip_prefix("surround")?;
            let mut it = d.chars().filter_map(|c| c.to_digit(10));
            let (x, y) = (it.next()?, it.next()?);
            if d.len() != 2 {
                return None;
            }
            (
                10 + x * 10 + y,
                format!("Analog Surround {x}.{y}{many}"),
                (x + y) as usize,
                None,
            )
        }
    };
    Some((
        rank,
        Port {
            label,
            name: name.to_owned(),
            channels,
            hdmi,
            plugged: None,
        },
    ))
}

/// The sound-server PCMs, in display order, with their labels.
const SERVER: [(&str, &str); 3] = [
    ("pipewire", "PipeWire"),
    ("pulse", "PulseAudio"),
    ("default", "ALSA default"),
];

/// The names cpal lists are ALSA's (Linux); on Windows and macOS every name is a plain device, all of them shown.
const ALSA: bool = cfg!(target_os = "linux");

fn raw_card() -> Card {
    blank(
        Kind::Raw,
        "",
        if ALSA {
            "Raw ALSA devices"
        } else {
            "Audio devices"
        },
    )
}

fn blank(kind: Kind, id: &str, title: &str) -> Card {
    Card {
        kind,
        id: id.to_owned(),
        title: title.to_owned(),
        note: String::new(),
        ports: Vec::new(),
        missing: false,
        busy: false,
    }
}

/// The cards behind `names` (cpal's list), the system default first. `show_all` adds every hidden name as an extra port
/// (under its card, or under a "Raw ALSA devices" card); `saved` is kept selectable whether or not it is listed: a hidden
/// name joins its card, an unlisted one becomes a [`Card::missing`] card.
pub fn group(names: &[String], show_all: bool, saved: &str) -> Vec<Card> {
    let mut default = blank(Kind::Default, "", "System default");
    default.ports.push(Port::plain("Default", ""));
    let mut server = blank(Kind::Server, "", "System sound server");
    let mut cards: Vec<(Card, Vec<(u32, Port)>)> = Vec::new();
    let mut raw = raw_card();
    for (key, label) in SERVER {
        if names.iter().any(|n| n == key) {
            server.ports.push(Port::plain(label, key));
        }
    }
    let wanted = |n: &str| !ALSA || show_all || (!saved.is_empty() && n == saved);
    for n in names {
        if SERVER.iter().any(|(k, _)| k == n) {
            continue;
        }
        let p = parse(n);
        let port = match (known(n, &p), p.card) {
            (Some(k), Some(_)) => Some(k),
            _ if wanted(n) => Some((1000, Port::plain(n, n))),
            _ => None,
        };
        let Some((rank, port)) = port else { continue };
        match p.card {
            Some(id) => {
                let at = cards
                    .iter()
                    .position(|(c, _)| c.id == id)
                    .unwrap_or_else(|| {
                        cards.push((blank(Kind::Card, id, id), Vec::new()));
                        cards.len() - 1
                    });
                cards[at].1.push((rank, port));
            }
            None => raw.ports.push(port),
        }
    }
    let mut out = vec![default];
    if !server.ports.is_empty() {
        out.push(server);
    }
    for (mut c, mut ports) in cards {
        ports.sort_by_key(|(r, _)| *r);
        c.ports = ports.into_iter().map(|(_, p)| p).collect();
        out.push(c);
    }
    if !raw.ports.is_empty() {
        out.push(raw);
    }
    if !saved.is_empty() && !out.iter().any(|c| c.find(saved).is_some()) {
        // Not listed: the device is gone, busy (cpal drops a PCM that is open, ours included), or never was. A name of a
        // card keeps its plain-words label; on a listed card it is one more port of it, a card that is not there becomes a
        // card of its own, marked missing; a name with no card goes to the raw card.
        let p = parse(saved);
        let id = p.card.unwrap_or_default();
        let port = known(saved, &p)
            .filter(|_| !id.is_empty())
            .map_or_else(|| Port::plain(saved, saved), |(_, port)| port);
        let kind = if id.is_empty() { Kind::Raw } else { Kind::Card };
        match out.iter_mut().find(|c| c.kind == kind && c.id == id) {
            Some(c) => {
                c.ports.push(port);
                if kind == Kind::Card {
                    c.ports
                        .sort_by_key(|p| known(&p.name, &parse(&p.name)).map_or(1000, |(r, _)| r));
                }
            }
            None => {
                let mut c = if kind == Kind::Raw {
                    raw_card()
                } else {
                    blank(Kind::Card, id, id)
                };
                c.ports.push(port);
                c.missing = true;
                out.push(c);
            }
        }
    }
    out
}

/// The names to add to `names` (cpal's list) for the playback cards the machine has: those of `seen` (earlier lists) that are
/// gone from it while their card is still there, and `sysdefault:CARD=<id>` for a card none of whose names was ever seen.
/// cpal lists only the PCMs it can open, and a PCM in use is held exclusively (measured on a USB headset: its card vanished from cpal's list as
/// soon as the system default played on it, and a HDA card's `surround40` once our own stream was on its `front`; cpal 0.15.3
/// `enumerate.rs` skips what `DeviceHandles::open` cannot open), so without this a card (or the port in use) would drop out of
/// the page while it plays.
pub(super) fn unlisted(names: &[String], seen: &[String], f: &Facts) -> Vec<String> {
    let there = |id: &str| f.cards.iter().any(|c| c.playback && c.id == id);
    let mut extra: Vec<String> = seen
        .iter()
        .filter(|n| !names.contains(n) && parse(n).card.is_some_and(there))
        .cloned()
        .collect();
    for c in f.cards.iter().filter(|c| c.playback) {
        let named = |n: &String| parse(n).card == Some(c.id.as_str());
        if !names.iter().chain(&extra).any(named) {
            extra.push(format!("sysdefault:CARD={}", c.id));
        }
    }
    extra
}

/// Titles, notes, HDMI connection state and the card order from what the machine says (see [`super::sys`]). Titles are made
/// unique with the card id ("HD-Audio Generic (Generic_1)").
pub(super) fn enrich(cards: &mut [Card], f: &Facts) {
    for c in cards.iter_mut() {
        match c.kind {
            Kind::Default => {
                let on = f
                    .default_sink
                    .as_deref()
                    .and_then(|d| f.sinks.iter().find(|s| s.name == d));
                c.note = on.map_or_else(String::new, |s| format!("plays on {}", s.description));
            }
            Kind::Card => {
                let sys = f.cards.iter().find(|s| s.id == c.id);
                if !c.missing {
                    c.title = f
                        .sink_of(&c.id)
                        .map(|s| s.description.clone())
                        .or_else(|| sys.and_then(|s| s.model.clone()))
                        .or_else(|| sys.map(|s| s.name.clone()))
                        .unwrap_or_else(|| c.id.clone());
                }
                c.note = sys.map_or_else(String::new, |s| format!("{} · card {}", s.driver, c.id));
                for p in &mut c.ports {
                    p.plugged = p
                        .hdmi
                        .and_then(|d| f.plugged.get(&(c.id.clone(), d)).copied());
                }
            }
            Kind::Server | Kind::Raw => {}
        }
    }
    let titles: Vec<String> = cards.iter().map(|c| c.title.clone()).collect();
    for (i, c) in cards.iter_mut().enumerate() {
        if c.kind == Kind::Card
            && titles
                .iter()
                .enumerate()
                .any(|(j, t)| j != i && *t == c.title)
        {
            c.title = format!("{} ({})", c.title, c.id);
        }
    }
    // Card order is the kernel's (a busy card is appended by `group` after the listed ones).
    let rank = |c: &Card| {
        let at = f
            .cards
            .iter()
            .position(|s| s.id == c.id)
            .unwrap_or(usize::MAX);
        (c.kind as u8, at)
    };
    cards.sort_by_key(rank);
}

/// Every selectable `(label, cpal name)` of `cards`, in list order: for a drop-down of devices (the input row).
pub fn choices(cards: &[Card]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in cards {
        for p in &c.ports {
            let label = match (c.kind, c.ports.len()) {
                (Kind::Default, _) => "SYSTEM DEFAULT".to_owned(),
                (Kind::Raw, _) => p.label.clone(),
                (_, 1) => c.title.clone(),
                _ => format!("{} · {}", c.title, p.text()),
            };
            out.push((
                if c.missing {
                    format!("{label} (not available)")
                } else {
                    label
                },
                p.name.clone(),
            ));
        }
    }
    out
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::audio::device::sys::{Sink, SysCard};

    /// The cpal output names of this machine (observed), a DJ controller plugged in.
    fn machine() -> Vec<String> {
        let mut v: Vec<String> = [
            "sysdefault",
            "lavrate",
            "samplerate",
            "speexrate",
            "jack",
            "pipewire",
            "pulse",
            "speex",
            "upmix",
            "vdownmix",
            "default",
            "sysdefault:CARD=A50",
            "front:CARD=A50,DEV=0",
            "surround40:CARD=A50,DEV=0",
            "iec958:CARD=A50,DEV=0",
            "hdmi:CARD=NVidia,DEV=0",
            "hdmi:CARD=NVidia,DEV=1",
            "hdmi:CARD=NVidia,DEV=2",
            "hdmi:CARD=NVidia,DEV=3",
            "sysdefault:CARD=Generic_1",
            "front:CARD=Generic_1,DEV=0",
            "surround21:CARD=Generic_1,DEV=0",
            "surround40:CARD=Generic_1,DEV=0",
            "surround41:CARD=Generic_1,DEV=0",
            "surround50:CARD=Generic_1,DEV=0",
            "surround51:CARD=Generic_1,DEV=0",
            "surround71:CARD=Generic_1,DEV=0",
            "iec958:CARD=Generic_1,DEV=0",
            "sysdefault:CARD=CTRL4X",
            "front:CARD=CTRL4X,DEV=0",
            "surround40:CARD=CTRL4X,DEV=0",
            "hw:CARD=A50,DEV=0",
            "plughw:CARD=A50,DEV=0",
            "dmix:CARD=A50,DEV=0",
        ]
        .map(String::from)
        .into();
        v.dedup();
        v
    }

    fn labels(c: &Card) -> Vec<String> {
        c.ports.iter().map(Port::text).collect()
    }

    #[test]
    fn names_are_taken_apart() {
        assert_eq!(
            parse("front:CARD=A50,DEV=0"),
            Parsed {
                plugin: "front",
                card: Some("A50"),
                dev: Some(0)
            }
        );
        assert_eq!(
            parse("sysdefault:CARD=CTRL4X"),
            Parsed {
                plugin: "sysdefault",
                card: Some("CTRL4X"),
                dev: None
            }
        );
        assert_eq!(
            parse("hw:0,1"),
            Parsed {
                plugin: "hw",
                card: Some("0"),
                dev: Some(1)
            }
        );
        assert_eq!(
            parse("lavrate"),
            Parsed {
                plugin: "lavrate",
                card: None,
                dev: None
            }
        );
    }

    #[test]
    fn plugins_and_converters_are_hidden_and_the_sound_server_is_one_device() {
        let g = group(&machine(), false, "");
        let keys: Vec<String> = g.iter().map(Card::key).collect();
        assert_eq!(
            keys,
            [
                "default",
                "server",
                "card:A50",
                "card:NVidia",
                "card:Generic_1",
                "card:CTRL4X"
            ]
        );
        let all: Vec<&str> = g
            .iter()
            .flat_map(|c| c.ports.iter().map(|p| p.name.as_str()))
            .collect();
        for hidden in [
            "lavrate",
            "samplerate",
            "speexrate",
            "speex",
            "upmix",
            "vdownmix",
            "jack",
            "hw:CARD=A50,DEV=0",
            "dmix:CARD=A50,DEV=0",
        ] {
            assert!(!all.contains(&hidden), "{hidden} must stay hidden");
        }
        assert_eq!(labels(&g[1]), ["PipeWire", "PulseAudio", "ALSA default"]);
        assert_eq!(g[1].ports[2].name, "default");
    }

    #[test]
    fn ports_are_named_in_plain_words_and_keep_the_exact_cpal_name() {
        let g = group(&machine(), false, "");
        assert_eq!(
            labels(&g[2]),
            [
                "Analog Output",
                "Default",
                "Analog Surround 4.0",
                "Digital Output (S/PDIF)"
            ]
        );
        assert_eq!(
            g[2].ports
                .iter()
                .map(|p| (p.name.as_str(), p.channels))
                .collect::<Vec<_>>(),
            [
                ("front:CARD=A50,DEV=0", 2),
                ("sysdefault:CARD=A50", 0),
                ("surround40:CARD=A50,DEV=0", 4),
                ("iec958:CARD=A50,DEV=0", 2)
            ]
        );
        assert_eq!(
            labels(&g[4]),
            [
                "Analog Output",
                "Default",
                "Analog Surround 2.1",
                "Analog Surround 4.0",
                "Analog Surround 4.1",
                "Analog Surround 5.0",
                "Analog Surround 5.1",
                "Analog Surround 7.1",
                "Digital Output (S/PDIF)"
            ]
        );
        assert_eq!(g[4].ports[6].channels, 6);
        assert_eq!(
            labels(&g[3]),
            [
                "HDMI / DisplayPort",
                "HDMI / DisplayPort 2",
                "HDMI / DisplayPort 3",
                "HDMI / DisplayPort 4"
            ]
        );
        assert_eq!(g[3].ports[1].name, "hdmi:CARD=NVidia,DEV=1");
    }

    #[test]
    fn show_all_lists_every_raw_name_under_its_card_or_the_raw_card() {
        let g = group(&machine(), true, "");
        let raw = g.last().unwrap();
        assert_eq!(raw.kind, Kind::Raw);
        assert_eq!(
            raw.ports
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            [
                "sysdefault",
                "lavrate",
                "samplerate",
                "speexrate",
                "jack",
                "speex",
                "upmix",
                "vdownmix"
            ]
        );
        let a50 = &g[2];
        assert!(
            a50.find("hw:CARD=A50,DEV=0").is_some() && a50.find("dmix:CARD=A50,DEV=0").is_some()
        );
        assert_eq!(
            a50.ports.last().unwrap().label,
            "dmix:CARD=A50,DEV=0",
            "raw ports are labelled with their raw name"
        );
        let mut every: Vec<&str> = g
            .iter()
            .flat_map(|c| c.ports.iter().map(|p| p.name.as_str()))
            .collect();
        every.retain(|n| !n.is_empty());
        for n in machine() {
            assert!(every.contains(&n.as_str()), "{n} is lost in show-all");
        }
    }

    #[test]
    fn an_old_saved_name_keeps_its_card_even_when_hidden_or_gone() {
        // A hidden raw name of a present card joins that card.
        let g = group(&machine(), false, "plughw:CARD=A50,DEV=0");
        let at = g
            .iter()
            .position(|c| c.find("plughw:CARD=A50,DEV=0").is_some())
            .unwrap();
        assert_eq!(g[at].key(), "card:A50");
        assert!(!g[at].missing);
        // A name of a card that is unplugged becomes a missing card, nothing else is selected.
        let names: Vec<String> = machine()
            .into_iter()
            .filter(|n| !n.contains("CTRL4X"))
            .collect();
        let g = group(&names, false, "sysdefault:CARD=CTRL4X");
        let last = g.last().unwrap();
        assert!(
            last.missing
                && last.key() == "card:CTRL4X"
                && last.find("sysdefault:CARD=CTRL4X") == Some(0)
        );
        // A card-less hidden name (a rate converter) goes to the raw card.
        let g = group(&machine(), false, "lavrate");
        assert!(g
            .last()
            .is_some_and(|c| c.kind == Kind::Raw && c.find("lavrate").is_some() && !c.missing));
        // The empty name is the System default card.
        assert_eq!(group(&machine(), false, "")[0].find(""), Some(0));
    }

    fn facts() -> Facts {
        Facts {
            sinks: vec![Sink {
                name: "alsa_output.usb-Logitech_A50-00.analog-stereo".into(),
                description: "A50 Analog Stereo".into(),
                alsa_id: Some("A50".into()),
                ..Sink::default()
            }],
            default_sink: Some("alsa_output.usb-Logitech_A50-00.analog-stereo".into()),
            cards: vec![
                SysCard {
                    num: 0,
                    id: "A50".into(),
                    driver: "USB-Audio".into(),
                    name: "A50".into(),
                    model: Some("Logitech A50".into()),
                    playback: true,
                },
                SysCard {
                    num: 1,
                    id: "NVidia".into(),
                    driver: "HDA-Intel".into(),
                    name: "HDA NVidia".into(),
                    model: Some("GA102 High Definition Audio Controller".into()),
                    playback: true,
                },
                SysCard {
                    num: 2,
                    id: "Generic".into(),
                    driver: "HDA-Intel".into(),
                    name: "HD-Audio Generic".into(),
                    model: None,
                    playback: true,
                },
                SysCard {
                    num: 3,
                    id: "Generic_1".into(),
                    driver: "HDA-Intel".into(),
                    name: "HD-Audio Generic".into(),
                    model: None,
                    playback: true,
                },
            ],
            plugged: [
                (("NVidia".to_owned(), 0), true),
                (("NVidia".to_owned(), 1), false),
            ]
            .into(),
        }
    }

    #[test]
    fn titles_prefer_the_sink_then_the_model_and_hdmi_shows_its_connection() {
        let mut names = machine();
        names.push("sysdefault:CARD=Generic".into());
        let mut g = group(&names, false, "");
        enrich(&mut g, &facts());
        let t: Vec<&str> = g.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(t[0], "System default");
        assert_eq!(g[0].note, "plays on A50 Analog Stereo");
        assert_eq!(t[2], "A50 Analog Stereo");
        assert_eq!(t[3], "GA102 High Definition Audio Controller");
        // Two cards named "HD-Audio Generic" are told apart by their id.
        assert_eq!(
            &t[4..7],
            [
                "HD-Audio Generic (Generic)",
                "HD-Audio Generic (Generic_1)",
                "CTRL4X"
            ]
        );
        let nv = &g[3];
        assert_eq!(nv.ports[0].text(), "HDMI / DisplayPort (plugged in)");
        assert_eq!(nv.ports[1].text(), "HDMI / DisplayPort 2 (unplugged)");
        assert_eq!(
            nv.ports[2].text(),
            "HDMI / DisplayPort 3",
            "unknown stays unmarked"
        );
        assert_eq!(nv.default_port(), 0);
    }

    #[test]
    fn a_click_on_an_hdmi_card_opens_the_connected_output() {
        let mut f = facts();
        f.plugged = [(("NVidia".to_owned(), 2), true)].into();
        let mut g = group(&machine(), false, "");
        enrich(&mut g, &f);
        assert_eq!(g[3].default_port(), 2);
        assert_eq!(
            g[2].default_port(),
            0,
            "an analog card opens its first port"
        );
    }

    #[test]
    fn a_card_the_sound_server_holds_stays_on_the_page_as_busy_in_kernel_order() {
        // cpal drops a PCM it cannot open: the A50 is gone from the list while the system default plays on it.
        let listed: Vec<String> = machine()
            .into_iter()
            .filter(|n| !n.contains("CARD=A50"))
            .collect();
        let mut f = facts();
        f.cards.retain(|c| c.id != "Generic"); // machine() has no names for it
                                               // Never seen before: only the card's own default stands in for it.
        assert_eq!(unlisted(&listed, &[], &f), ["sysdefault:CARD=A50"]);
        assert!(
            unlisted(&machine(), &machine(), &f).is_empty(),
            "a listed card is not added again"
        );
        // Seen before: all its names come back (7 of the machine's names belong to the A50).
        let extra = unlisted(&listed, &machine(), &f);
        assert!(extra.iter().all(|n| n.contains("CARD=A50")) && extra.len() == 7);
        let mut names = listed.clone();
        names.extend(extra);
        let mut g = group(&names, false, "");
        enrich(&mut g, &f);
        assert_eq!(
            g.iter().map(Card::key).collect::<Vec<_>>(),
            [
                "default",
                "server",
                "card:A50",
                "card:NVidia",
                "card:Generic_1",
                "card:CTRL4X"
            ]
        );
        assert_eq!(g[2].title, "A50 Analog Stereo");
        assert_eq!(
            labels(&g[2]),
            [
                "Analog Output",
                "Default",
                "Analog Surround 4.0",
                "Digital Output (S/PDIF)"
            ]
        );
        // A stored port of a present card that was never listed joins it with its plain label and its place.
        let g = group(&names, false, "surround51:CARD=A50,DEV=0");
        assert_eq!(g.iter().filter(|c| c.id == "A50").count(), 1);
        let a50 = g.iter().find(|c| c.id == "A50").unwrap();
        assert_eq!(a50.find("surround51:CARD=A50,DEV=0"), Some(3));
        assert_eq!(a50.ports[3].label, "Analog Surround 5.1");
    }

    #[test]
    fn choices_label_devices_for_a_drop_down() {
        let mut g = group(&machine(), false, "sysdefault:CARD=GONE");
        enrich(&mut g, &facts());
        let c = choices(&g);
        assert_eq!(c[0], ("SYSTEM DEFAULT".to_owned(), String::new()));
        assert!(c.contains(&(
            "System sound server · PipeWire".to_owned(),
            "pipewire".to_owned()
        )));
        assert!(c.contains(&(
            "A50 Analog Stereo · Analog Output".to_owned(),
            "front:CARD=A50,DEV=0".to_owned()
        )));
        assert_eq!(
            c.last().unwrap(),
            &(
                "GONE (not available)".to_owned(),
                "sysdefault:CARD=GONE".to_owned()
            )
        );
    }
}
