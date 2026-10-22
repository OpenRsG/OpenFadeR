//! MIDI controllers ↔ ui/midi.slint (global `MidiState`). Input ports are opened by [`port`] (all of
//! them, or those whose name contains `OPENFADER_MIDI_PORT`, which also picks the LED output); midir's thread only pushes raw
//! messages into a FIFO and coalesces event-loop wakes; bounded UI-thread batches dispatch them, so Slint state stays on the UI thread.
//!
//! Bindings come from two places, the user's winning: [`usermap`] (`midi.map`, written by MIDI LEARN)
//! and [`mapfile`] (the mapping file of the connected controller). The actions are the table in [`actions`]. Status is the note-on
//! (`0x9n`) or CC (`0xBn`) byte with the channel in `n`; a note-on with velocity 0 is a note-off
//! (release). Press actions fire on note-on or CC ≥ 64, knobs scale CC 0..127 to 0..1 (14-bit pairs:
//! CC `n` = MSB, CC `n+0x20` = LSB, applied on the LSB, when the mapping says `hires`) and relative
//! encoders decode to signed ticks ([`rel_2c`] / [`rel_64`]). Jog wheels go through [`jog`] to the deck's
//! scratch / bend API. LEDs follow the app state through [`led`] on the controller's output port, change
//! only; the messages leave through a [`port::Sink`] thread, one per `<outInterval>` ms of the mapping file, so the
//! 258-light start-up state is not one burst. The jog ring's playhead refreshes every `<ringInterval>` ms, the
//! rest of the state every 50 ms; they go dark when the [`Guard`] returned by [`wire`] is dropped.
//! Browser: the encoder walks the tracks, BACK puts it on the playlist tree, push (Forward) returns to the tracks
//! or loads the chosen track into the first idle deck, the SORT buttons sort the browser (again = reverse).
//! Channel / cross fader START (the controller sends them when its fader-start setting is on) start and stop
//! the deck. Shift layers: RELATED (View+Shift), memory-cue delete, sampler slot stop, pad pages (PAD FX effects I to P, beat-jump sizes,
//! keyboard / key-shift semitones), KEYBOARD + SHIFT pad = delete the hot cue, SAMPLE
//! SCRATCH + SHIFT pad = unload the track, FX1 / FX2 units (the deck column of an FX action is the unit). Channel VU goes out as CC 0..127
//! on the 50 ms state tick, only when it changes. SIGINT / SIGTERM close the window cleanly, so the LEDs go dark.
//!
//! Deck 3/4 (a 4-deck controller): its deck 3 / 4 layer sends on MIDI channels 3 / 4, so
//! those bindings drive decks C / D directly; the deck-select inputs say which deck of each side is selected
//! ([`layer`]; the controller lights its own DECK buttons),
//! and only that deck's lights are sent (a layer just selected is sent in full). DECK 3 / 4 widen a 2-deck layout to 4 decks;
//! other controls of a hidden deck are ignored with a status note. SHIFT + DECK toggles
//! DUAL DECK of a side: its transport, loop, cue and pad controls and the jog go to both decks of the pair ([`input`]).
//! JOG CUTTER needs VINYL on and runs on one deck at a time ([`jog::cutters`]).
//! `OPENFADER_MIDI_LOG=1` prints every incoming message with the action it hit, or `unmapped`.
//!
//! MIDI LEARN: the MIDI chip opens an overlay; the next control moved is captured (the live message
//! is not dispatched meanwhile) and clicking an action row (A / B deck) binds it and rewrites midi.map.
//! Hot-plug, controller state and audio: [`hotplug`] (a thread polls the port list), [`session`] (what a change of ports does: open ports, load the
//! mapping, start the lights, ask the controller for its state, move the audio output to its interface, "connected" / "disconnected"
//! toasts) and [`dump`] (the state reply: faders, knobs and deck layer land in the app as if moved). One mapped controller per run.
//!
//! Children of this base: [`input`] (one decoded message: LEARN capture, state dump, dispatch to the binding's action), [`wheel`]
//! (jog and platter state), [`learn`] (binding a captured control), [`lights`] (the light timer and signals), [`clock`] (the MIDI
//! clock output that external gear follows), [`wire`] (set-up and the input wakes and jog/housekeeping timer). They add methods to
//! [`Midi`]; the types they share live here. [`get`] is the way in for the repository catalog
//! (`app/catalog`): mapping files read, installed into the config folder, and the session opened again with one.

mod actions;
mod clock;
mod dump;
pub mod get;
mod hotplug;
mod input;
mod jog;
mod layer;
mod learn;
mod led;
mod lights;
mod mapfile;
mod port;
mod session;
mod usermap;
mod wheel;
mod wire;

use super::{
    deck::{Decks, DECKS, DECK_NAMES},
    App,
};
use jog::Jog;
use led::Leds;
use port::Sink;
use slint::Timer;
use std::{
    cell::Cell,
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    rc::Rc,
    time::Instant,
};

/// A channel message without its value: status byte and data1.
type Msg = (u8, u8);

pub use wire::wire;

/// PREFS "Use controller audio when connected".
fn prefs_audio() -> bool {
    super::prefs::with(|p| p.controller_audio)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Binding {
    status: u8,
    d1: u8,
    /// Index into [`actions::ACTIONS`].
    act: usize,
    /// 0..3 = deck A..D (the controller's deck 1..4 layer); for the FX / MERGE actions the unit / side (0 or 1).
    deck: i32,
    /// The mapping row's constant (pad slot, pad mode…); 0 for user bindings.
    arg: i32,
    /// 14-bit knob: `d1` is the MSB controller, `d1 + 0x20` the LSB.
    hires: bool,
}

/// What a controller light shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Led {
    Play,
    Cue,
    Sync,
    Master,
    Loop,
    /// JOG CUTTER mode on.
    Cutter,
    Vinyl,
    HpCue,
    Loaded,
    /// Pad mode button (0 hot cue, 1 beat jump, 2 keyboard, 3 beat loop, 4 pad FX, 5 key shift).
    Mode(i32),
    /// Pad (mode, slot).
    Pad(i32, usize),
    /// Pad `slot` of whatever mode the deck shows (controllers whose pads send the same note in every mode).
    PadSlot(usize),
    Sampler,
    /// Beat FX slot (unit 0 / 1, slot 0..2) on.
    Fx(usize, usize),
    /// Beat FX slot (unit, slot) has effect (selector index), and with `true` also is on: `FX1-1Select.ECHO` / `FX1-1On.ECHO`.
    FxEffect(usize, usize, i32, bool),
    /// Beat FX unit's tempo from TAP (`true`, `FX1TapBeat`) or AUTO (`false`, `FX1AutoBeat`).
    FxTapped(usize, bool),
    /// Beat FX unit's assignment is (0..3 CH1..CH4, 4 MASTER): `FX1Assign2`, `FX1AssignMaster`.
    FxAssign(usize, i32),
    /// Beat FX slot (unit, slot) on, with the unit on channel (0..3): `CH1_FX1-2OnMomentary`.
    FxChOn(usize, i32, usize),
    /// Beat FX unit 0 / 1 RELEASE FX running.
    FxRelease(usize),
    /// Beat FX unit 0 / 1 beat display (its BEAT target slot): velocity = the beat's code, not on / off ([`led::beat_code`]).
    FxBeat(usize),
    /// Sampler pad `slot` of the visible bank holds a sample.
    SamplerPad(usize),
    /// Shift-layer lights: TEMPO RANGE (not ±6 %), JUMP TO TRACK START (deck loaded), TAP BPM (GRID edit), 4 BEAT LOOP (active loop is 4 beats).
    TempoRange,
    TrackStart,
    Tap,
    Loop4,
    /// Channel VU meter: velocity = level 0..127, not on / off.
    Level,
    /// Jog ring: segment 1..=72 of the playhead's turn (0 = no track).
    Ring,
    /// MERGE FX side 0 / 1: preset `n` (0..4) selected / effect held on.
    MergePreset(usize, usize),
    MergeOn(usize),
    /// STEMS: ACTIVE PART light of part 0..3 (VOCAL, INST, BASS, DRUMS: lit while heard), the PART ISOLATOR mode light, and FX PART SELECT
    /// light of part 1..4 (lit while the selected Beat FX unit's effect runs on it).
    Stem(usize),
    Iso,
    FxPart(i32),
    /// BEAT LAB: DRUM SWAP pad 0..3 of the bank in view playing or waiting, BANK A (0) / B (1) in view, MULTI (true) / SINGLE mode, DRUM CAPTURE armed.
    Drum(usize),
    DrumBank(usize),
    DrumMode(bool),
    DrumCapture,
}

/// One feedback message: the light `led` of deck `deck` is the (status, data1) `msg`; `blink` = the period in ms of the light's
/// blinking state (a light without it only ever shows on / off).
#[derive(Clone, Debug, PartialEq)]
struct LedRow {
    led: Led,
    deck: usize,
    msg: Msg,
    blink: Option<u64>,
}

/// A channel message; a note-on with velocity 0 is already rewritten to its note-off.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ev {
    status: u8,
    d1: u8,
    d2: u8,
}

impl Ev {
    /// The control's key in the bindings: a release is looked up as the note-on it belongs to.
    fn key(self) -> (u8, u8) {
        let off = if self.status & 0xF0 == 0x80 { 0x10 } else { 0 };
        (self.status | off, self.d1)
    }

    fn is_off(self) -> bool {
        self.status & 0xF0 == 0x80
    }

    /// A press: a note-on, or a CC at 64 or above.
    fn on(self) -> bool {
        match self.status & 0xF0 {
            0x90 => true,
            0xB0 => self.d2 >= 64,
            _ => false,
        }
    }

    /// The MSB control of a 14-bit pair when this is its LSB (CC `n + 0x20`).
    fn msb_key(self) -> Option<(u8, u8)> {
        (self.status & 0xF0 == 0xB0 && self.d1 & 0xE0 == 0x20)
            .then(|| (self.status, self.d1 - 0x20))
    }

    /// Relative value as signed ticks: two's complement, or centred on 64.
    fn ticks(self, two_comp: bool) -> i32 {
        if two_comp {
            rel_2c(self.d2)
        } else {
            rel_64(self.d2)
        }
    }
}

fn decode(b: [u8; 3]) -> Option<Ev> {
    let [mut status, d1, d2] = b;
    if (d1 | d2) & 0x80 != 0 || !matches!(status & 0xF0, 0x80 | 0x90 | 0xB0) {
        return None;
    }
    if status & 0xF0 == 0x90 && d2 == 0 {
        status &= 0x8F;
    }
    Some(Ev { status, d1, d2 })
}

/// Relative CC, two's complement: 1..63 forward, 127..65 back (127 = −1).
fn rel_2c(v: u8) -> i32 {
    if v < 64 {
        v as i32
    } else {
        v as i32 - 128
    }
}

/// Relative CC centred on 64: 65 = +1, 63 = −1.
fn rel_64(v: u8) -> i32 {
    v as i32 - 64
}

/// A 14-bit controller value (MSB, LSB) as 0..1.
fn combine14(msb: u8, lsb: u8) -> f32 {
    ((msb as u32) << 7 | lsb as u32) as f32 / 16383.0
}

/// "CC 7 ch 1" / "Note 36 ch 2" for a control key.
fn describe((status, d1): (u8, u8)) -> String {
    format!(
        "{} {d1} ch {}",
        if status & 0xF0 == 0xB0 { "CC" } else { "Note" },
        (status & 0x0F) + 1
    )
}

/// What a message did, for the log and the LEARN status line.
enum Outcome {
    /// Action index and deck.
    Done(usize, i32),
    /// MSB of a 14-bit pair: applied when the LSB arrives.
    Half,
    Ignored(i32),
    Unmapped,
    /// A report of the controller's state dump ([`dump`]) and what became of it.
    Dump(&'static str),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Outcome::Done(act, deck) => write!(
                f,
                "{} {}",
                actions::ACTIONS[*act].name,
                DECK_NAMES[*deck as usize]
            ),
            Outcome::Half => f.write_str("14-bit MSB held"),
            Outcome::Ignored(deck) => write!(f, "deck {} layer ignored (2-deck layout)", deck + 1),
            Outcome::Unmapped => f.write_str("unmapped"),
            Outcome::Dump(why) => write!(f, "state dump: {why}"),
        }
    }
}

/// The session state every child works on: bindings, jog and light state, ports. UI thread only (`RefCell` / `Cell`).
struct Midi {
    app: slint::Weak<App>,
    decks: Decks,
    /// The user's own bindings (saved to midi.map), looked up before the mapping file's.
    user: RefCell<Vec<Binding>>,
    /// The connected controller's mapping-file bindings and the action name of every input message (for the log); empty without one ([`session`]).
    file_binds: RefCell<HashMap<(u8, u8), Binding>>,
    file_names: RefCell<HashMap<(u8, u8), String>>,
    /// Control captured by LEARN, waiting for an action.
    captured: Cell<Option<(u8, u8)>>,
    /// Last MSB seen per 14-bit control.
    msb: RefCell<HashMap<(u8, u8), u8>>,
    jog: RefCell<[Jog; DECKS]>,
    /// PREFS jog rotation speed, kept here so the per-tick path does not clone the settings.
    jog_factor: Cell<f64>,
    vinyl: Cell<[bool; DECKS]>,
    cutter: Cell<[bool; DECKS]>,
    /// The deck selected on each side of the controller ([`layer`]), DUAL DECK per side (left, right; [`Midi::toggle_dual`]) and
    /// when the lights started (blink phase).
    layer: Cell<layer::Layer>,
    dual: Cell<[bool; 2]>,
    started: Instant,
    log: bool,
    layer34_noted: Cell<bool>,
    out: RefCell<Option<Sink>>,
    leds: RefCell<Leds>,
    /// The MIDI clock output (PREFS > CONTROLLER > MIDI CLOCK OUT), `None` while that setting is empty ([`clock`]).
    clock: RefCell<Option<clock::Clock>>,
    /// What the lights show: the whole Slint state every [`lights::STATE_EVERY`], the jog ring's playheads on every light tick.
    snap: RefCell<led::Snap>,
    snap_at: Cell<Option<Instant>>,
    /// `OPENFADER_MIDI_PORT`: only ports whose name contains it are used (and watched).
    filter: Option<String>,
    /// The open input ports and their message channel, and the port list they were opened for.
    ports: RefCell<Option<port::Ports>>,
    names: RefCell<Vec<String>>,
    watcher: RefCell<Option<hotplug::Watcher>>,
    /// The connected controller that has a mapping file, if any ([`session`]).
    link: RefCell<Option<session::Link>>,
    /// The state dump's reply in progress ([`dump`]).
    dump: Cell<Option<dump::Window>>,
    /// The request waiting for the first tick ([`Midi::start_dump`]).
    dump_due: RefCell<Option<Vec<u8>>>,
    /// Note-ons not yet released: on unplug they are released for the controller.
    held: RefCell<HashSet<(u8, u8)>>,
    /// Toasts waiting for the next tick ([`Midi::notify`]).
    pending: RefCell<Vec<String>>,
    /// The light timer, restarted when a controller with another jog-ring period connects.
    lights: Timer,
}

/// Keeps the controller connected; dropping it (end of `run`) turns the LEDs off and closes the ports.
#[must_use = "dropping the guard closes the controller; keep it until the window is closed"]
pub struct Guard {
    midi: Rc<Midi>,
    _input: Rc<Timer>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        *self.midi.watcher.borrow_mut() = None; // stops the hot-plug thread
        self.midi.close(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_velocity_zero_is_off() {
        assert_eq!(
            decode([0x91, 36, 0]),
            Some(Ev {
                status: 0x81,
                d1: 36,
                d2: 0
            })
        );
        assert_eq!(
            decode([0x91, 36, 90]),
            Some(Ev {
                status: 0x91,
                d1: 36,
                d2: 90
            })
        );
        assert_eq!(decode([0xB0, 7, 127]).unwrap().status, 0xB0);
        assert_eq!(decode([0xE0, 1, 1]), None, "pitch bend is not mapped");
        assert_eq!(decode([0x90, 0x80, 1]), None, "data bytes are 7-bit");
    }

    #[test]
    fn release_keys_as_its_note_on_and_cc_presses_from_64() {
        let off = decode([0x92, 36, 0]).unwrap();
        assert_eq!(
            (off.key(), off.is_off(), off.on()),
            ((0x92, 36), true, false)
        );
        let cc = |v| decode([0xB1, 7, v]).unwrap();
        assert_eq!((cc(63).on(), cc(64).on()), (false, true));
        assert!(
            !decode([0x80, 36, 100]).unwrap().on(),
            "a note-off velocity is not a press"
        );
    }

    #[test]
    fn lsb_names_its_msb_control() {
        assert_eq!(
            decode([0xB2, 0x33, 5]).unwrap().msb_key(),
            Some((0xB2, 0x13))
        );
        assert_eq!(decode([0xB2, 0x13, 5]).unwrap().msb_key(), None);
        assert_eq!(
            decode([0x92, 0x33, 5]).unwrap().msb_key(),
            None,
            "notes have no LSB"
        );
    }

    #[test]
    fn relative_cc_decodes() {
        assert_eq!(
            [1, 5, 63, 127, 126, 65].map(rel_2c),
            [1, 5, 63, -1, -2, -63]
        );
        assert_eq!([64, 65, 63, 0, 127].map(rel_64), [0, 1, -1, -64, 63]);
    }

    #[test]
    fn fourteen_bit_pairs_combine() {
        assert_eq!(combine14(0, 0), 0.0);
        assert_eq!(combine14(127, 127), 1.0);
        let mid = combine14(64, 0);
        assert!((mid - 8192.0 / 16383.0).abs() < 1e-6);
        assert!(
            combine14(10, 127) < combine14(11, 0),
            "the MSB dominates, the LSB fine-tunes"
        );
    }

    #[test]
    fn describe_names_the_control() {
        assert_eq!(describe((0xB0, 7)), "CC 7 ch 1");
        assert_eq!(describe((0x91, 36)), "Note 36 ch 2");
    }
}
