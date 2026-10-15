//! The output device cards of the AUDIO page ↔ `PrefsState.audio-cards` (ui/prefs-audio-card.slint).
//!
//! One card per device from [`device::cards`]: the green check marks the stored choice, the status says whether it is
//! in use, overridden by the controller / `OPENFADER_OUTPUT_DEVICE`, or missing (the system default plays instead). A click
//! on a card or its Port drop-down comes back as `choose("out:<card key>", port)` and stores that port's cpal name, which
//! reopens the stream. Volume and mute are not settings: the sliders call `PrefsState.audio-volume` / `audio-mute`, which act
//! on the device through [`volume`] and patch the card in place (the page is not rebuilt, so a drag is not interrupted).
//!
//! The model is kept in step by [`sync`]: after every change, and by a 250 ms timer while the page is open (a refresh of the
//! device lists when the page opens, then every [`PERIOD`] to pick up a hot-plugged card or a volume changed elsewhere; not
//! right after the user's own volume edit).

mod level;
mod sync;

use sync::Job;
pub(super) use sync::sync;

use crate::app::{AudioCard, PrefsState};
use openfader::audio::{
    Status,
    device::{
        self,
        card::{Card, Kind},
        volume::{self, Level, Target},
    },
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    time::Instant,
};

/// Which card the check is on, which one plays, and the status line of a card.
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    selected: bool,
    in_use: bool,
    /// The port the drop-down shows: the stored one, else the one playing, else the card's default.
    port: usize,
    status: String,
    /// 0 none, 1 in use, 2 not what was chosen, 3 problem.
    kind: i32,
}

/// What each card shows. `saved` is the stored `output_device`, `st` what the stream open decided, `controller` the connected
/// controller whose interface wins when `st.source` says so.
fn marks(cards: &[Card], saved: &str, st: &Status, controller: Option<&str>) -> Vec<Mark> {
    let selected = cards.iter().position(|c| c.find(saved).is_some());
    let used = match st.source {
        "settings" => selected,
        "system default" => cards.iter().position(|c| c.kind == Kind::Default),
        "OPENFADER_OUTPUT_DEVICE" | "controller" => {
            cards.iter().position(|c| c.find(&st.device).is_some())
        }
        _ => None,
    };
    cards
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (sel, playing) = (selected == Some(i), used == Some(i));
            let (status, kind) = match (sel, playing, st.source) {
                (true, true, _) => ("IN USE".to_owned(), 1),
                (true, false, _) if c.missing => {
                    ("NOT CONNECTED · THE SYSTEM DEFAULT PLAYS".to_owned(), 3)
                }
                (true, false, "controller") => (
                    format!(
                        "OVERRIDDEN BY {}",
                        controller.unwrap_or("CONTROLLER").to_uppercase()
                    ),
                    2,
                ),
                (true, false, "OPENFADER_OUTPUT_DEVICE") => {
                    ("OVERRIDDEN BY OPENFADER_OUTPUT_DEVICE".to_owned(), 2)
                }
                (true, false, "system default") if c.busy => {
                    ("BUSY · THE SYSTEM DEFAULT PLAYS".to_owned(), 3)
                }
                (true, false, "system default") => {
                    ("COULD NOT OPEN · THE SYSTEM DEFAULT PLAYS".to_owned(), 3)
                }
                (true, false, _) => ("SELECTED".to_owned(), 0),
                (false, true, "controller") => ("IN USE · CONTROLLER AUDIO".to_owned(), 2),
                (false, true, "OPENFADER_OUTPUT_DEVICE") => {
                    ("IN USE · OPENFADER_OUTPUT_DEVICE".to_owned(), 2)
                }
                (false, true, _) => ("IN USE · FALLBACK".to_owned(), 2),
                (false, false, _) if c.busy => ("IN USE BY THE SOUND SERVER".to_owned(), 0),
                _ => (String::new(), 0),
            };
            let port = if sel {
                c.find(saved)
            } else if playing {
                c.find(&st.device)
            } else {
                None
            };
            Mark {
                selected: sel,
                in_use: playing,
                port: port.unwrap_or_else(|| c.default_port()),
                status,
                kind,
            }
        })
        .collect()
}

/// The cpal name stored when port `i` of the card keyed `key` is chosen; `None` for an unknown card or port.
pub(super) fn pick(cards: &[Card], key: &str, i: usize) -> Option<String> {
    let c = cards.iter().find(|c| c.key() == key)?;
    c.ports.get(i).map(|p| p.name.clone())
}

/// What a volume control of this card acts on.
fn target(c: &Card) -> Option<Target> {
    match c.kind {
        Kind::Default | Kind::Server => Some(Target::Sink),
        Kind::Card if !c.missing => Some(Target::Alsa(c.id.clone())),
        _ => None,
    }
}

fn no_volume(c: &Card) -> &'static str {
    match c.kind {
        _ if c.missing => "Not connected.",
        Kind::Raw => "No volume control for raw ALSA devices.",
        Kind::Card => {
            "No hardware volume control (a card played directly bypasses the sound server's volume)."
        }
        _ => "No volume control: the sound server does not answer (pactl).",
    }
}

/// Advanced lines of a card: the exact ALSA name, channels, rates, buffer sizes and, while it plays, the latency.
fn details(c: &Card, port: usize, in_use: Option<String>, caps: bool) -> Vec<String> {
    let Some(p) = c.ports.get(port) else {
        return Vec::new();
    };
    let mut d = vec![format!(
        "ALSA name: {}",
        if p.name.is_empty() {
            "(the system default)"
        } else {
            p.name.as_str()
        }
    )];
    if c.missing {
        d.push("Not connected.".to_owned());
        return d;
    }
    let found = if caps && c.kind != Kind::Raw {
        device::output_caps(&p.name)
    } else {
        None
    };
    match found {
        Some(k) => {
            d.push(format!("Channels: {}", k.channels));
            let list = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
            d.push(format!(
                "Sample rates: {} Hz",
                if k.rates.is_empty() {
                    "not reported".to_owned()
                } else {
                    list(&k.rates)
                }
            ));
            if let (Some(lo), Some(hi)) = (k.buffers.first(), k.buffers.last()) {
                d.push(format!("Buffer sizes: {lo} – {hi} frames"));
            }
        }
        None if p.channels > 0 => d.push(format!("Channels: {}", p.channels)),
        None => {}
    }
    d.extend(in_use.map(|l| format!("Latency: {l}")));
    d
}

/// Everything one card shows, in plain Rust (compared to skip unchanged rows; converted by [`slint_card`]).
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    key: String,
    title: String,
    note: String,
    mark: Mark,
    icon: i32,
    ports: Vec<String>,
    target: Option<Target>,
    level: Option<Level>,
    volume_note: String,
    details: Vec<String>,
}

thread_local! {
    /// What the model shows now.
    static SHOWN: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    /// Cards whose Advanced section is open (their rates are probed).
    static OPEN: RefCell<HashSet<String>> = RefCell::default();
    /// The user's last volume edit.
    static EDITED: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn build(j: &Job) -> Vec<Entry> {
    let st = &j.st;
    let cards = device::cards(false, j.all, &j.saved);
    let marks = marks(&cards, &j.saved, st, j.controller.as_deref());
    let mut sink: Option<Option<Level>> = None; // Default and Server share one read
    cards
        .iter()
        .zip(marks)
        .map(|(c, mark)| {
            let t = target(c);
            let level = match &t {
                Some(Target::Sink) => sink
                    .get_or_insert_with(|| volume::read(&Target::Sink))
                    .clone(),
                Some(t) => volume::read(t),
                None => None,
            };
            let latency = (mark.in_use && j.frames > 0 && st.rate > 0).then(|| {
                format!(
                    "~{:.1} ms ({} frames at {} Hz)",
                    device::latency_ms(j.frames, st.rate),
                    j.frames,
                    st.rate
                )
            });
            let key = format!("out:{}", c.key());
            let probe = (mark.selected || mark.in_use || j.open.contains(&key)) && !c.busy;
            Entry {
                key,
                title: c.title.clone(),
                note: c.note.clone(),
                icon: match c.kind {
                    Kind::Server => 2,
                    Kind::Raw => 3,
                    _ if c.ports.iter().any(|p| p.hdmi.is_some()) => 1,
                    _ => 0,
                },
                ports: c.ports.iter().map(|p| p.text()).collect(),
                details: details(c, mark.port, latency, probe),
                volume_note: no_volume(c).to_owned(),
                level,
                target: t,
                mark,
            }
        })
        .collect()
}

fn slint_card(e: &Entry) -> AudioCard {
    let strings = |v: &[String]| {
        ModelRc::new(VecModel::from(
            v.iter()
                .map(|s| SharedString::from(s.as_str()))
                .collect::<Vec<_>>(),
        ))
    };
    let l = e.level.as_ref();
    AudioCard {
        key: e.key.as_str().into(),
        title: e.title.as_str().into(),
        note: e.note.as_str().into(),
        status: e.mark.status.as_str().into(),
        status_kind: e.mark.kind,
        icon: e.icon,
        selected: e.mark.selected,
        ports: strings(&e.ports),
        port: e.mark.port as i32,
        volume_ok: l.is_some(),
        volume: l.map_or(0.0, |l| l.percent as f32 / 100.0),
        volume_text: l.map_or_else(SharedString::new, |l| l.text().into()),
        volume_max: l.map_or_else(SharedString::new, |l| l.max_text().into()),
        volume_note: e.volume_note.as_str().into(),
        muted: l.is_some_and(|l| l.muted),
        can_mute: l.is_some_and(|l| l.can_mute),
        details: strings(&e.details),
    }
}

fn app() -> Option<crate::app::App> {
    crate::app::prefs::dialog::app()
}

/// Connects the callbacks and the page watcher; call after `prefs::wire` has stored the window.
pub(super) fn wire() {
    let Some(app) = app() else { return };
    let s = app.global::<PrefsState>();
    s.on_audio_volume(|k, v| level::set_volume(&k, v.clamp(0, 100) as u32));
    s.on_audio_mute(|k, on| level::set_mute(&k, on));
    s.on_audio_advanced(|k, on| {
        OPEN.with_borrow_mut(|o| {
            if on {
                o.insert(k.to_string());
            } else {
                o.remove(k.as_str());
            }
        });
        sync::sync();
    });
    sync::watch();
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::audio::device::card::group;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn machine() -> Vec<String> {
        names(&[
            "sysdefault",
            "lavrate",
            "pipewire",
            "pulse",
            "default",
            "sysdefault:CARD=A50",
            "front:CARD=A50,DEV=0",
            "surround40:CARD=A50,DEV=0",
            "hdmi:CARD=NVidia,DEV=0",
            "hdmi:CARD=NVidia,DEV=1",
            "sysdefault:CARD=CTRLX6",
            "front:CARD=CTRLX6,DEV=0",
            "surround40:CARD=CTRLX6,DEV=0",
        ])
    }

    fn status(source: &'static str, device: &str) -> Status {
        Status {
            device: device.to_owned(),
            source,
            rate: 48_000,
            channels: 2,
            ..Status::default()
        }
    }

    fn texts(m: &[Mark]) -> Vec<&str> {
        m.iter().map(|m| m.status.as_str()).collect()
    }

    #[test]
    fn the_check_follows_the_stored_name_and_the_old_names_still_find_their_card() {
        let cards = group(&machine(), false, "surround40:CARD=A50,DEV=0");
        let m = marks(
            &cards,
            "surround40:CARD=A50,DEV=0",
            &status("settings", "surround40:CARD=A50,DEV=0"),
            None,
        );
        let at = m.iter().position(|m| m.selected).unwrap();
        assert_eq!(cards[at].key(), "card:A50");
        assert_eq!(cards[at].ports[m[at].port].label, "Analog Surround 4.0");
        assert_eq!(m.iter().filter(|m| m.selected).count(), 1);
        assert_eq!(m[at].status, "IN USE");
        // "" is the System default card; with nothing stored it is the selected one and the one playing.
        let cards = group(&machine(), false, "");
        let m = marks(&cards, "", &status("system default", "default"), None);
        assert!(m[0].selected && m[0].in_use && m[0].status == "IN USE");
        assert!(m[1..].iter().all(|m| !m.selected && m.status.is_empty()));
    }

    #[test]
    fn a_missing_device_says_so_and_the_default_is_marked_as_the_fallback() {
        let gone: Vec<String> = machine()
            .into_iter()
            .filter(|n| !n.contains("CTRLX6"))
            .collect();
        let cards = group(&gone, false, "sysdefault:CARD=CTRLX6");
        let m = marks(
            &cards,
            "sysdefault:CARD=CTRLX6",
            &status("system default", "default"),
            None,
        );
        let last = cards.len() - 1;
        assert!(cards[last].missing && m[last].selected && m[last].kind == 3);
        assert_eq!(m[last].status, "NOT CONNECTED · THE SYSTEM DEFAULT PLAYS");
        assert_eq!((m[0].status.as_str(), m[0].kind), ("IN USE · FALLBACK", 2));
    }

    #[test]
    fn a_controller_or_the_environment_overrides_the_choice_and_the_card_that_plays_shows_it() {
        let cards = group(&machine(), false, "sysdefault:CARD=A50");
        let st = status("controller", "surround40:CARD=CTRLX6,DEV=0");
        let m = marks(&cards, "sysdefault:CARD=A50", &st, Some("controller"));
        let sel = cards.iter().position(|c| c.key() == "card:A50").unwrap();
        let used = cards.iter().position(|c| c.key() == "card:CTRLX6").unwrap();
        assert_eq!(
            (m[sel].status.as_str(), m[sel].kind),
            ("OVERRIDDEN BY CONTROLLER", 2)
        );
        assert_eq!(m[used].status, "IN USE · CONTROLLER AUDIO");
        assert_eq!(
            cards[used].ports[m[used].port].label, "Analog Surround 4.0",
            "the drop-down shows the port that plays"
        );
        let m = marks(
            &cards,
            "sysdefault:CARD=A50",
            &status("OPENFADER_OUTPUT_DEVICE", "front:CARD=CTRLX6,DEV=0"),
            None,
        );
        assert_eq!(m[sel].status, "OVERRIDDEN BY OPENFADER_OUTPUT_DEVICE");
        assert_eq!(
            texts(&m).iter().filter(|t| t.starts_with("IN USE")).count(),
            1
        );
    }

    #[test]
    fn a_chosen_card_that_could_not_open_says_why() {
        // The stored A50 port is not offered while the sound server holds the card; the stream fell back to the default.
        let mut cards = group(&machine(), false, "front:CARD=A50,DEV=0");
        let a50 = cards.iter().position(|c| c.id == "A50").unwrap();
        let st = status("system default", "default");
        let m = marks(&cards, "front:CARD=A50,DEV=0", &st, None);
        assert_eq!(m[a50].status, "COULD NOT OPEN · THE SYSTEM DEFAULT PLAYS");
        cards[a50].busy = true;
        let m = marks(&cards, "front:CARD=A50,DEV=0", &st, None);
        assert_eq!(
            (m[a50].status.as_str(), m[a50].kind),
            ("BUSY · THE SYSTEM DEFAULT PLAYS", 3)
        );
        // Not chosen, a busy card only says who holds it.
        let m = marks(&cards, "", &status("system default", "default"), None);
        assert_eq!(m[a50].status, "IN USE BY THE SOUND SERVER");
    }

    #[test]
    fn choosing_a_card_or_a_port_stores_the_exact_cpal_name() {
        let cards = group(&machine(), false, "");
        assert_eq!(pick(&cards, "default", 0).as_deref(), Some(""));
        assert_eq!(pick(&cards, "server", 0).as_deref(), Some("pipewire"));
        assert_eq!(
            pick(&cards, "card:A50", 2).as_deref(),
            Some("surround40:CARD=A50,DEV=0")
        );
        assert_eq!(
            pick(&cards, "card:NVidia", 1).as_deref(),
            Some("hdmi:CARD=NVidia,DEV=1")
        );
        assert_eq!(
            (pick(&cards, "card:GONE", 0), pick(&cards, "card:A50", 9)),
            (None, None)
        );
    }

    #[test]
    fn volume_follows_the_card_kind() {
        let cards = group(&machine(), true, "");
        let t: Vec<Option<Target>> = cards.iter().map(target).collect();
        assert_eq!(t[0], Some(Target::Sink));
        assert_eq!(t[1], Some(Target::Sink));
        assert_eq!(t[2], Some(Target::Alsa("A50".to_owned())));
        assert_eq!(*t.last().unwrap(), None, "raw plugins have no mixer");
    }

    #[test]
    fn advanced_lists_the_raw_name_and_what_the_name_promises() {
        let cards = group(&machine(), false, "");
        let a50 = cards.iter().find(|c| c.id == "A50").unwrap();
        let d = details(
            a50,
            2,
            Some("~10.7 ms (512 frames at 48000 Hz)".into()),
            false,
        );
        assert_eq!(
            d,
            [
                "ALSA name: surround40:CARD=A50,DEV=0",
                "Channels: 4",
                "Latency: ~10.7 ms (512 frames at 48000 Hz)"
            ]
        );
        assert_eq!(
            details(&cards[0], 0, None, false),
            ["ALSA name: (the system default)"]
        );
    }
}
