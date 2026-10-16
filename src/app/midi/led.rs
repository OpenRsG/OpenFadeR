//! LED feedback: the controller's lights follow the on-screen state. [`snap`] reads the Slint globals into a plain
//! [`Snap`], [`lit`] says whether a [`Led`] is on, and [`Leds`] turns that into MIDI messages, sending only what
//! changed since the last call (the first call therefore sends the whole current state).
//! Only the selected deck of each side of the controller is sent to ([`super::layer`]); a deck whose layer was just
//! selected is sent in full. A light whose output has `blink="<ms>"` alternates between on and off with that period
//! while it [`blinking`]: the SYNC light blinks while a synced PLAY waits for the master's next beat. The first half
//! of each period is on.
//! Spacing the messages by `<outInterval>` is the sender's job ([`super::port::Sink`]).
//!
//! Message = the row's output (status, data1) with velocity 0x7F on / 0x00 off (the usual feedback is a note-on; VU
//! and jog-ring lights and the Beat FX beat display are CCs with a value).
//! ponytail: the mapping carries no pad colours, so pads are plain on/off; colour velocities need the controller's MIDI
//! spec or a hardware check.

use super::{
    App, Led, LedRow,
    jog::REV_S,
    layer::{self, Layer},
};
use crate::app::{
    DeckState, FxSlot, FxState, FxUnit, GrooveState, MixerState, SamplerState,
    deck::{DECKS, PadMode},
};
use openfader::audio::fx::BEATS;
use slint::{ComponentHandle, Model};
use std::collections::HashMap;

const ON: u8 = 0x7F;
/// Jog ring refresh in ms when the mapping file has no `<ringInterval>`: 12 is the commonest value
/// among the shipped files (32 occurs too).
pub const RING_MS: u64 = 12;

#[derive(Clone, Default, PartialEq, Debug)]
pub struct DeckSnap {
    loaded: bool,
    playing: bool,
    /// Paused on a cue point or previewing it (CUE LED).
    at_cue: bool,
    synced: bool,
    /// A synced PLAY waits for the master's next beat (SYNC blinks).
    sync_armed: bool,
    master: bool,
    loop_active: bool,
    pad_mode: i32,
    pads: [bool; 8],
    /// PAD FX pad held, 0..16 (8..16 = the SHIFT page): the PAD FX lights follow it, not the 8 pads in view. Set by the light
    /// tick (`lights.rs`) from the deck, not by [`snap`].
    pub pad_fx: Option<usize>,
    hp_cue: bool,
    /// Tempo range other than the start-up ±6 % (TEMPO RANGE light).
    range_changed: bool,
    grid_edit: bool,
    loop_beats: i32,
    /// Channel VU, 0..127.
    level: u8,
    /// Playhead, track ms (jog ring).
    pub pos_ms: f64,
    /// STEMS: parts at full level (VOCAL, INST, BASS, DRUMS; none until the stems are ready) and the channel's PART ISOLATOR mode.
    stem_on: [bool; 4],
    iso: bool,
    /// BEAT LAB: the bank in view's 4 slots playing or waiting for the beat, the bank in view, MULTI mode, DRUM CAPTURE armed.
    drum: [bool; 4],
    drum_bank: i32,
    drum_multi: bool,
    drum_capture: bool,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Snap {
    pub decks: [DeckSnap; DECKS],
    vinyl: [bool; DECKS],
    cutter: [bool; DECKS],
    merge_preset: [i32; 2],
    merge_on: [bool; 2],
    /// Beat FX slots FX1-1 … FX2-3: ON, and effect (selector index).
    fx_on: [[bool; 3]; 2],
    fx_effect: [[i32; 3]; 2],
    fx_release: [bool; 2],
    /// The unit's tempo comes from TAP (else AUTO, the master deck's).
    fx_tapped: [bool; 2],
    /// Each unit's assignment: 0..3 CH1..CH4, 4 MASTER.
    fx_assign: [i32; 2],
    /// FX PART SELECT of the selected Beat FX unit: 0 ALL, 1 VOCAL … 4 DRUMS.
    fx_part: i32,
    /// BEAT selector position (index into `BEATS`) of each unit's BEAT target slot: the controller's beat display.
    fx_beat: [usize; 2],
    sampler_open: bool,
    /// Loaded state of the visible sampler bank's 8 pads.
    sampler: [bool; 8],
    /// The deck selected on each side of the controller.
    pub layer: Layer,
    /// Milliseconds since the lights started: the blink phase.
    pub now_ms: u64,
}

/// `f` of each of a unit's three slots (defaults without the unit).
fn slots_of<T: Default + Copy>(u: &Option<FxUnit>, f: impl Fn(&FxSlot) -> T) -> [T; 3] {
    std::array::from_fn(|i| {
        u.as_ref()
            .and_then(|u| u.slots.row_data(i))
            .map_or(T::default(), |s| f(&s))
    })
}

/// Decks the layout hides (2-deck mode) read as empty: their lights stay dark.
pub fn snap(app: &App, vinyl: [bool; DECKS], cutter: [bool; DECKS], pos_ms: [f64; DECKS]) -> Snap {
    let (ds, mx) = (app.global::<DeckState>(), app.global::<MixerState>());
    let fx_state = app.global::<FxState>();
    let units = fx_state.get_units();
    let fx = [units.row_data(0), units.row_data(1)];
    let sp = app.global::<SamplerState>();
    let (decks, channels, groove) = (
        ds.get_decks(),
        mx.get_channels(),
        app.global::<GrooveState>().get_decks(),
    );
    let bank = sp.get_bank() as usize * 8;
    let slots = sp.get_slots();
    let mut s = Snap {
        vinyl,
        cutter,
        merge_preset: fx
            .each_ref()
            .map(|u| u.as_ref().map_or(0, |u| u.merge_preset)),
        merge_on: fx
            .each_ref()
            .map(|u| u.as_ref().is_some_and(|u| u.merge_on)),
        fx_on: fx.each_ref().map(|u| slots_of(u, |s| s.on)),
        fx_effect: fx.each_ref().map(|u| slots_of(u, |s| s.effect)),
        fx_release: fx
            .each_ref()
            .map(|u| u.as_ref().is_some_and(|u| u.releasing)),
        fx_tapped: fx.each_ref().map(|u| u.as_ref().is_some_and(|u| u.tapped)),
        fx_assign: fx.each_ref().map(|u| u.as_ref().map_or(4, |u| u.assign)),
        fx_part: units
            .row_data(fx_state.get_unit().max(0) as usize)
            .map_or(0, |u| u.part),
        fx_beat: fx.each_ref().map(|u| {
            u.as_ref()
                .and_then(|u| u.slots.row_data(u.target.max(0) as usize))
                .map_or(0, |s| s.beat.max(0) as usize)
        }),
        sampler_open: sp.get_open(),
        sampler: std::array::from_fn(|n| slots.row_data(bank + n).is_some_and(|v| v.loaded)),
        ..Snap::default()
    };
    for (i, d) in s
        .decks
        .iter_mut()
        .enumerate()
        .take(ds.get_count().max(0) as usize)
    {
        let Some(v) = decks.row_data(i) else {
            continue;
        };
        let g = groove.row_data(i).unwrap_or_default();
        let ch = channels.row_data(i);
        *d = DeckSnap {
            loaded: v.loaded,
            playing: v.playing,
            at_cue: v.at_cue,
            synced: v.synced,
            sync_armed: v.sync_armed,
            master: v.master,
            loop_active: v.loop_active,
            pad_mode: v.pad_mode,
            pads: std::array::from_fn(|n| v.pads.row_data(n).is_some_and(|p| p.set)),
            hp_cue: ch.as_ref().is_some_and(|c| c.cue),
            range_changed: v.tempo_range != "±6%", // deck/transport.rs RANGES[0], the start-up range
            grid_edit: v.grid_edit,
            loop_beats: v.loop_beats,
            // VU 0..1 → CC 0..127; the clamp keeps the cast in range.
            level: ch
                .as_ref()
                .map_or(0, |c| (c.level.clamp(0.0, 1.0) * 127.0).round() as u8),
            pos_ms: pos_ms[i],
            stem_on: std::array::from_fn(|n| {
                v.stem_ready && v.stem_full.row_data(n).unwrap_or(false)
            }),
            iso: ch.as_ref().is_some_and(|c| c.iso),
            drum: std::array::from_fn(|n| {
                g.slots
                    .row_data(g.bank.max(0) as usize * 4 + n)
                    .is_some_and(|s| s.state > 0)
            }),
            drum_bank: g.bank,
            drum_multi: g.multi,
            drum_capture: g.capture_on,
            pad_fx: None, // `lights.rs` fills it on every tick
        };
    }
    s
}

/// Is `led` of deck `d` on?
fn lit(led: Led, d: usize, s: &Snap) -> bool {
    let k = &s.decks[d];
    match led {
        Led::Play => k.playing,
        Led::Cue => k.at_cue,
        Led::Sync => k.synced,
        Led::Master => k.master,
        Led::Loop => k.loop_active,
        Led::Cutter => s.cutter[d],
        Led::Vinyl => s.vinyl[d],
        Led::HpCue => k.hp_cue,
        Led::Loaded => k.loaded,
        Led::Mode(m) => k.pad_mode == m,
        // Pads of the other modes go dark: only the visible mode's pad states are known.
        // PAD FX lights both pages (the SHIFT page is its own 8 lights); the other modes' pads are the 8 in view.
        Led::Pad(m, slot) if m == PadMode::PadFx as i32 => {
            k.pad_mode == m && k.pad_fx == Some(slot)
        }
        Led::Pad(m, slot) => k.pad_mode == m && k.pads[slot],
        Led::PadSlot(slot) => k.pads[slot],
        Led::Sampler => s.sampler_open,
        Led::Fx(u, i) => s.fx_on[u][i],
        Led::FxEffect(u, i, e, on) => s.fx_effect[u][i] == e && (!on || s.fx_on[u][i]),
        Led::FxTapped(u, tapped) => s.fx_tapped[u] == tapped,
        Led::FxAssign(u, a) => s.fx_assign[u] == a,
        Led::FxChOn(u, ch, i) => s.fx_assign[u] == ch && s.fx_on[u][i],
        Led::FxRelease(u) => s.fx_release[u],
        Led::SamplerPad(slot) => s.sampler[slot],
        Led::TempoRange => k.range_changed,
        Led::TrackStart => k.loaded,
        Led::Tap => k.grid_edit,
        Led::Loop4 => k.loop_active && k.loop_beats == 4,
        Led::Level => k.level > 0,
        Led::Ring => k.loaded,
        Led::MergePreset(side, n) => s.merge_preset[side] == n as i32,
        Led::MergeOn(side) => s.merge_on[side],
        Led::Stem(p) => k.stem_on[p],
        Led::Iso => k.iso,
        Led::FxPart(p) => s.fx_part == p,
        Led::FxBeat(_) => true,
        Led::Drum(n) => k.drum[n],
        Led::DrumBank(b) => k.drum_bank == b as i32,
        Led::DrumMode(multi) => k.drum_multi == multi,
        Led::DrumCapture => k.drum_capture,
    }
}

/// A SYNC light whose start is armed shows its blinking state instead of on / off.
fn blinking(led: Led, d: usize, s: &Snap) -> bool {
    led == Led::Sync && s.decks[d].sync_armed
}

/// The blinking phase at `now_ms` for a `period_ms` cycle: on in the first half.
fn blink_on(now_ms: u64, period_ms: u64) -> bool {
    now_ms % period_ms.max(1) * 2 < period_ms
}

/// Jog ring segment 1..=72 of a playhead at `ms`: one turn per
/// [`REV_S`] like the platter model (1 = 12 o'clock, clockwise).
fn ring(ms: f64) -> u8 {
    // rem_euclid of a tiny negative position can round up to exactly 1.0, which would make segment 73.
    (1 + ((ms / 1000.0 / REV_S).rem_euclid(1.0) * 72.0) as u8).min(72)
}

/// The value of the Beat FX beat display for BEAT selector position `i` (index into `BEATS`): `0xB4 0x64 dd` (FX1) /
/// `0xB5 0x64 dd` (FX2). 1/16 = 1, 1/8 = 2, 1/4 = 3,
/// 1/2 = 4, 1 = 5, 2 = 6 … 32 = 0x0A, and 3/4 its own 0x21.
pub fn beat_code(i: usize) -> u8 {
    const CODES: [u8; BEATS.len()] = [1, 2, 3, 4, 0x21, 5, 6, 7, 8, 9, 0x0A];
    CODES[i.min(CODES.len() - 1)]
}

/// The velocity a light sends: VU meters their level, the jog ring its segment, every other light on / off.
fn value(led: Led, d: usize, s: &Snap) -> u8 {
    match led {
        Led::Level => s.decks[d].level,
        Led::Ring if s.decks[d].loaded => ring(s.decks[d].pos_ms),
        Led::FxBeat(u) => beat_code(s.fx_beat[u]),
        _ if lit(led, d, s) => ON,
        _ => 0,
    }
}

/// The connect light show: for [`INTRO_SWEEP`] ms a comet runs through the on / off lights of
/// every MIDI channel at once (each channel's lights in note order) while the jog rings spin in opposite directions, the VU
/// meters bounce and the beat displays count up; then everything flashes on for [`INTRO_FLASH`] ms and the live state takes
/// over (only the lights that differ from the flash are sent). 2 s: the comet switches each light on and off once, and at the
/// usual 2 ms `<outInterval>` a 500-light controller takes about that long for those 1000 messages (measured on a
/// virtual port: shorter sweeps only queue up in the sink).
const INTRO_SWEEP: u64 = 2000;
const INTRO_FLASH: u64 = 400;
/// The comet's length, as a share of its channel.
const TAIL: f64 = 0.3;
/// `intro` before the first tick has seen the clock.
const INTRO_DUE: u64 = u64::MAX;

/// The velocity of `led` (deck `deck`) at `t` ms into the show; `u` is the light's place 0..1 along its channel's comet.
fn intro_value(led: Led, deck: usize, u: f64, t: u64) -> u8 {
    let flash = t >= INTRO_SWEEP;
    match led {
        Led::Ring if flash => 72,
        Led::Ring => {
            let seg = ((t * 216 / INTRO_SWEEP + deck as u64 * 18) % 72) as u8; // three turns
            if deck % 2 == 0 { seg + 1 } else { 72 - seg }
        }
        Led::Level if flash => 127,
        Led::Level => {
            let phase = t as f64 / 400.0 * std::f64::consts::PI + deck as f64;
            ((phase.sin().abs() * 7.0).round() * 18.0).min(127.0) as u8 // 8 steps: fewer messages than a smooth bounce
        }
        Led::FxBeat(_) if flash => 0x0A,
        Led::FxBeat(_) => 3 + (t / 200 % 8) as u8,
        _ if flash => ON,
        _ => {
            let head = t as f64 / INTRO_SWEEP as f64 * (1.0 + TAIL);
            if (head - TAIL..head).contains(&u) {
                ON
            } else {
                0
            }
        }
    }
}

pub struct Leds {
    rows: Vec<LedRow>,
    /// Last velocity sent per (status, data1).
    sent: HashMap<(u8, u8), u8>,
    /// The layer the last update saw.
    layer: Layer,
    /// When the connect show started (`now_ms`; [`INTRO_DUE`] = on the next update), and each light's place along its comet.
    intro: Option<(u64, HashMap<(u8, u8), f64>)>,
}

impl Leds {
    pub fn new(rows: Vec<LedRow>) -> Leds {
        Leds {
            rows,
            sent: HashMap::new(),
            layer: Layer::default(),
            intro: None,
        }
    }

    /// Plays the connect light show from the next update on.
    pub fn intro(&mut self) {
        let mut msgs: Vec<(u8, u8)> = self.rows.iter().map(|r| r.msg).collect();
        (msgs.sort_unstable(), msgs.dedup());
        let place = msgs
            .iter()
            .map(|&(status, d1)| {
                let channel = msgs.iter().filter(|m| m.0 == status);
                let rank = channel.clone().filter(|m| m.1 < d1).count();
                ((status, d1), rank as f64 / channel.count() as f64)
            })
            .collect();
        self.intro = Some((INTRO_DUE, place));
    }

    /// Sends one message per LED of a selected deck whose state differs from the last one sent; the lights of a deck
    /// that has just become the selected one of its side are sent whatever was sent before. While the connect show
    /// runs it is what is sent.
    pub fn update(&mut self, s: &Snap, mut send: impl FnMut([u8; 3])) {
        let before = std::mem::replace(&mut self.layer, s.layer);
        if before != s.layer {
            for r in self
                .rows
                .iter()
                .filter(|r| layer::selected(s.layer, r.deck) && !layer::selected(before, r.deck))
            {
                self.sent.remove(&r.msg);
            }
        }
        if let Some((start, _)) = &mut self.intro {
            if *start == INTRO_DUE {
                *start = s.now_ms;
            }
            if s.now_ms.saturating_sub(*start) >= INTRO_SWEEP + INTRO_FLASH {
                self.intro = None;
            }
        }
        for r in self
            .rows
            .iter()
            .filter(|r| layer::selected(s.layer, r.deck))
        {
            let v = match (&self.intro, r.blink) {
                (Some((start, place)), _) => intro_value(
                    r.led,
                    r.deck,
                    place.get(&r.msg).copied().unwrap_or(0.0),
                    s.now_ms - start,
                ),
                (None, Some(ms)) if blinking(r.led, r.deck, s) => {
                    if blink_on(s.now_ms, ms) {
                        ON
                    } else {
                        0
                    }
                }
                _ => value(r.led, r.deck, s),
            };
            if self.sent.insert(r.msg, v) != Some(v) {
                send([r.msg.0, r.msg.1, v]);
            }
        }
    }

    /// Everything off (exit): only lights that are on are touched.
    pub fn off(&mut self, mut send: impl FnMut([u8; 3])) {
        for (&(status, d1), v) in self.sent.iter_mut().filter(|(_, v)| **v != 0) {
            *v = 0;
            send([status, d1, 0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_show_lights_every_light_then_hands_over_to_the_live_state() {
        let mut leds = Leds::new(vec![
            row(Led::Play, 0, (0x90, 0x0B)),
            row(Led::Play, 1, (0x91, 0x0B)),
            row(Led::Pad(0, 2), 0, (0x97, 0x02)),
            row(Led::Pad(3, 2), 0, (0x97, 0x62)),
        ]);
        let mut s = Snap::default();
        s.decks[1].playing = true;
        drain(&mut leds, &s);
        leds.intro();
        let mut lit = std::collections::HashSet::new();
        for t in (0..INTRO_SWEEP).step_by(12) {
            s.now_ms = 1000 + t;
            lit.extend(
                drain(&mut leds, &s)
                    .into_iter()
                    .filter(|m| m[2] == ON)
                    .map(|m| (m[0], m[1])),
            );
        }
        assert_eq!(lit.len(), 4, "the comet passes every light");
        s.now_ms = 1000 + INTRO_SWEEP;
        drain(&mut leds, &s);
        assert!(leds.sent.values().all(|&v| v == ON), "flash");
        s.now_ms = 1000 + INTRO_SWEEP + INTRO_FLASH;
        assert_eq!(
            drain(&mut leds, &s),
            [[0x90, 0x0B, 0], [0x97, 0x02, 0], [0x97, 0x62, 0]],
            "the live state comes back: only deck 2's PLAY stays lit"
        );
    }

    fn row(led: Led, deck: usize, msg: (u8, u8)) -> LedRow {
        LedRow {
            led,
            deck,
            msg,
            blink: None,
        }
    }

    fn rows() -> Vec<LedRow> {
        vec![
            row(Led::Play, 0, (0x90, 0x0B)),
            row(Led::Play, 1, (0x91, 0x0B)),
            row(Led::Pad(0, 2), 0, (0x97, 0x02)),
            row(Led::Pad(3, 2), 0, (0x97, 0x62)),
        ]
    }

    /// What one `update` sends for `s`.
    fn drain(leds: &mut Leds, s: &Snap) -> Vec<[u8; 3]> {
        let mut out = Vec::new();
        leds.update(s, |m| out.push(m));
        out
    }

    #[test]
    fn sends_only_changes_and_the_whole_state_first() {
        let mut leds = Leds::new(rows());
        let mut s = Snap::default();
        assert_eq!(
            drain(&mut leds, &s).len(),
            4,
            "startup sends every LED (off included)"
        );
        assert!(
            drain(&mut leds, &s).is_empty(),
            "unchanged state sends nothing"
        );
        s.decks[0].playing = true;
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x0B, ON]]);
        s.decks[0].pads[2] = true;
        assert_eq!(
            drain(&mut leds, &s),
            [[0x97, 0x02, ON]],
            "only the visible pad mode lights (mode 0)"
        );
        s.decks[0].pad_mode = 3;
        assert_eq!(
            drain(&mut leds, &s),
            [[0x97, 0x02, 0], [0x97, 0x62, ON]],
            "switching mode moves the light"
        );
    }

    /// "PLAY went dark right after starting the deck": the light is the player's state, so it goes dark exactly when
    /// the deck stops (the null audio device runs a track out in well under a second), and the ring then rests
    /// on the last playhead segment.
    #[test]
    fn play_light_follows_the_deck_through_track_end() {
        let mut leds = Leds::new(vec![
            row(Led::Play, 0, (0x90, 0x0B)),
            row(Led::Ring, 0, (0xBB, 0x00)),
        ]);
        let mut s = Snap::default();
        s.decks[0] = DeckSnap {
            loaded: true,
            ..DeckSnap::default()
        };
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x0B, 0], [0xBB, 0x00, 1]]);
        s.decks[0].playing = true;
        s.decks[0].pos_ms = 900.0;
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x0B, ON], [0xBB, 0x00, 37]]);
        s.decks[0].pos_ms = 1800.0 * 200.0 + 450.0; // 200 turns later, a quarter round the ring
        assert_eq!(
            drain(&mut leds, &s),
            [[0xBB, 0x00, 19]],
            "the light stays on while the deck plays"
        );
        s.decks[0].playing = false; // end of track
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x0B, 0]]);
    }

    #[test]
    fn exit_turns_lit_leds_off_once() {
        let mut leds = Leds::new(rows());
        let mut s = Snap::default();
        s.decks[1].playing = true;
        drain(&mut leds, &s);
        let mut out = Vec::new();
        leds.off(|m| out.push(m));
        assert_eq!(out, [[0x91, 0x0B, 0]]);
        out.clear();
        leds.off(|m| out.push(m));
        assert!(out.is_empty());
    }

    #[test]
    fn vu_level_is_sent_as_its_value_and_only_when_it_changes() {
        let mut leds = Leds::new(vec![row(Led::Level, 1, (0xB0, 0x02))]);
        let mut s = Snap::default();
        let mut out = drain(&mut leds, &s);
        s.decks[1].level = 90;
        out.extend(drain(&mut leds, &s));
        out.extend(drain(&mut leds, &s));
        s.decks[1].level = 0;
        out.extend(drain(&mut leds, &s));
        assert_eq!(out, [[0xB0, 0x02, 0], [0xB0, 0x02, 90], [0xB0, 0x02, 0]]);
    }

    #[test]
    fn jog_ring_follows_the_playhead_one_turn_per_revolution() {
        let mut s = Snap::default();
        assert_eq!(value(Led::Ring, 0, &s), 0, "no track, no ring");
        s.decks[0] = DeckSnap {
            loaded: true,
            ..DeckSnap::default()
        };
        let seg = |ms: f64, s: &mut Snap| {
            s.decks[0].pos_ms = ms;
            value(Led::Ring, 0, s)
        };
        assert_eq!(
            [seg(0.0, &mut s), seg(900.0, &mut s), seg(1799.0, &mut s)],
            [1, 37, 72],
            "1..=72 around one 1.8 s turn"
        );
        assert_eq!(seg(1800.0, &mut s), 1, "wraps");
        assert_eq!(
            seg(-10.0, &mut s),
            72,
            "negative positions still land on the ring"
        );
    }

    #[test]
    fn pads_light_only_in_their_mode() {
        // (PAD FX lights follow the held pad instead, see `pad_fx_lights_follow_the_held_pad_…`)
        let mut s = Snap::default();
        s.decks[0].pad_mode = 3;
        s.decks[0].pads[1] = true;
        assert!(
            lit(Led::Pad(3, 1), 0, &s)
                && !lit(Led::Pad(5, 1), 0, &s)
                && !lit(Led::Pad(0, 1), 0, &s)
        );
        s.decks[0].pad_mode = 5;
        assert!(lit(Led::Pad(5, 1), 0, &s) && !lit(Led::Pad(3, 1), 0, &s));
        // a pad that sends the same note in every mode lights in whatever mode shows
        assert!(lit(Led::PadSlot(1), 0, &s) && !lit(Led::PadSlot(2), 0, &s));
    }

    #[test]
    fn loop4_needs_an_active_four_beat_loop() {
        let mut s = Snap::default();
        s.decks[0] = DeckSnap {
            loop_active: true,
            loop_beats: 4,
            ..DeckSnap::default()
        };
        assert!(lit(Led::Loop4, 0, &s) && !lit(Led::Loop4, 1, &s));
        s.decks[0].loop_beats = 8;
        assert!(!lit(Led::Loop4, 0, &s));
    }

    /// All four decks' rows, on their own channels.
    fn four_decks() -> Leds {
        Leds::new(
            (0..4)
                .map(|d| row(Led::Play, d, (0x90 + d as u8, 0x0B)))
                .collect(),
        )
    }

    #[test]
    fn only_the_selected_deck_of_each_side_is_sent_to() {
        let mut leds = four_decks();
        let mut s = Snap::default();
        s.decks[2].playing = true; // deck C plays behind the layer A / B
        assert_eq!(
            drain(&mut leds, &s),
            [[0x90, 0x0B, 0], [0x91, 0x0B, 0]],
            "C and D are not selected: nothing for them"
        );
        // DECK 3 on the left: deck C's lights are sent in full (PLAY on), deck A's stop
        s.layer = [1, 0];
        assert_eq!(drain(&mut leds, &s), [[0x92, 0x0B, ON]]);
        s.decks[0].playing = true; // A plays behind C now: not sent
        assert!(drain(&mut leds, &s).is_empty());
        // back to deck 1: A's state is sent in full, even though it is unchanged since the first send
        s.layer = [0, 0];
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x0B, ON]]);
        // DECK 4 on the right
        s.layer = [0, 1];
        s.decks[3].playing = true;
        assert_eq!(drain(&mut leds, &s), [[0x93, 0x0B, ON]]);
    }

    #[test]
    fn a_blinking_light_alternates_with_its_period() {
        assert!(
            [0, 299].iter().all(|&t| blink_on(t, 600))
                && [300, 599].iter().all(|&t| !blink_on(t, 600))
                && blink_on(600, 600)
        );
        let mut leds = Leds::new(vec![
            LedRow {
                led: Led::Sync,
                deck: 0,
                msg: (0x90, 0x58),
                blink: Some(600),
            },
            row(Led::Sync, 1, (0x91, 0x58)),
        ]);
        let mut s = Snap::default();
        s.decks[0] = DeckSnap {
            synced: true,
            ..DeckSnap::default()
        };
        s.decks[1] = DeckSnap {
            synced: true,
            ..DeckSnap::default()
        };
        assert_eq!(
            drain(&mut leds, &s),
            [[0x90, 0x58, ON], [0x91, 0x58, ON]],
            "synced: steady on"
        );
        s.decks[0].sync_armed = true;
        s.decks[1].sync_armed = true;
        s.now_ms = 100;
        assert!(
            drain(&mut leds, &s).is_empty(),
            "armed, first half of the period: still on"
        );
        s.now_ms = 350;
        assert_eq!(
            drain(&mut leds, &s),
            [[0x90, 0x58, 0]],
            "armed, second half: off; a row without Blink= never blinks"
        );
        s.now_ms = 650;
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x58, ON]]);
        s.decks[0].sync_armed = false; // the start fired: steady on again
        s.now_ms = 950;
        assert!(
            drain(&mut leds, &s).is_empty(),
            "steady on is what was last sent"
        );
        s.decks[0].synced = false;
        assert_eq!(drain(&mut leds, &s), [[0x90, 0x58, 0]]);
    }

    #[test]
    fn beat_display_codes_and_only_changes_are_sent() {
        let code = |beat: f32| beat_code(BEATS.iter().position(|&b| b == beat).unwrap());
        for (beat, dd) in [
            (0.0625, 1),
            (0.125, 2),
            (0.25, 3),
            (0.5, 4),
            (0.75, 0x21),
            (1.0, 5),
            (2.0, 6),
            (4.0, 7),
            (8.0, 8),
            (16.0, 9),
            (32.0, 0x0A),
        ] {
            assert_eq!(code(beat), dd, "{beat}");
        }

        let row = |unit, status| LedRow {
            led: Led::FxBeat(unit),
            deck: 0,
            msg: (status, 0x64),
            blink: None,
        };
        let mut leds = Leds::new(vec![row(0, 0xB4), row(1, 0xB5)]);
        let mut s = Snap {
            fx_beat: [5, 8],
            ..Snap::default()
        }; // 1 beat, 8 beats
        assert_eq!(drain(&mut leds, &s), [[0xB4, 0x64, 5], [0xB5, 0x64, 8]]);
        assert!(drain(&mut leds, &s).is_empty());
        s.fx_beat[0] = 4; // ◁ on FX1: 3/4
        assert_eq!(drain(&mut leds, &s), [[0xB4, 0x64, 0x21]]);
    }

    #[test]
    fn pad_fx_lights_follow_the_held_pad_on_either_page_and_only_in_that_mode() {
        let fx = PadMode::PadFx as i32;
        let mut s = Snap::default();
        s.decks[0] = DeckSnap {
            pad_mode: fx,
            pad_fx: Some(9), // pad 2 of the SHIFT page (effect J)
            ..DeckSnap::default()
        };
        assert!(lit(Led::Pad(fx, 9), 0, &s));
        assert!(
            !lit(Led::Pad(fx, 1), 0, &s),
            "pad 2 of page 1 is not pad 10"
        );
        assert!(!lit(Led::Pad(fx, 9), 1, &s), "another deck");
        s.decks[0].pad_mode = PadMode::HotCue as i32;
        assert!(
            !lit(Led::Pad(fx, 9), 0, &s),
            "the PAD FX lights go dark outside their mode"
        );
    }
}
