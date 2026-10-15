//! Controller state dump: on connect the app asks the controller to report where every fader, knob and switch is, so the
//! software matches the hardware (without it a control only syncs on its first move).
//!
//! The request is the mapping file's `<stateRequest>`, a system-exclusive message sent through the output port; the controller
//! answers with ordinary channel messages, the same ones
//! the controls send when moved (typically about 100 messages in under 100 ms, 14-bit pairs MSB then LSB, then the button
//! lights' states). They arrive through the normal input path while the dump [`Window`] is open and [`plan`] decides what each one
//! may do, so a state report is never taken for a button press:
//! knobs and faders apply (the binding's own action, 14-bit pairs combined as usual), the DECK layer lights select the layer,
//! VINYL / JOG CUTTER states set the mode; every other button state is skipped.
//! A mapping file without `<stateRequest>` sends nothing: its values sync on the first move.

use super::{
    Binding, Ev,
    actions::{ACTIONS, Kind},
};
use std::time::{Duration, Instant};

/// How long messages count as the reply: the first must come within [`FIRST`], then the reply ends after [`QUIET`] without one, and
/// never lasts more than [`MAX`] from the request.
const FIRST: Duration = Duration::from_millis(500);
const QUIET: Duration = Duration::from_millis(150);
const MAX: Duration = Duration::from_millis(1500);

/// A reply in progress, from the request on.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    started: Instant,
    last: Instant,
    /// Replies seen, and how many of them changed something.
    pub seen: u32,
    pub applied: u32,
}

impl Window {
    pub fn new(now: Instant) -> Window {
        Window {
            started: now,
            last: now,
            seen: 0,
            applied: 0,
        }
    }

    pub fn open(&self, now: Instant) -> bool {
        let quiet = if self.seen == 0 { FIRST } else { QUIET };
        now.duration_since(self.started) < MAX && now.duration_since(self.last) < quiet
    }

    /// A reply arrived at `now`; `applied`: it changed the software's state.
    pub fn saw(&mut self, now: Instant, applied: bool) {
        (self.last, self.seen, self.applied) = (now, self.seen + 1, self.applied + applied as u32);
    }
}

/// What a state report does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// A knob or fader (or half of a 14-bit pair): through its binding's action.
    Knob,
    /// The controller's deck `d` is the selected one on its side.
    Layer(usize),
    Vinyl(usize, bool),
    Cutter(usize, bool),
    /// Not a state we take over (why).
    Skip(&'static str),
}

/// Decides what the report `ev` is. `find` looks a control up in the bindings, `shown` = decks the layout shows (a hidden deck's
/// controls are skipped silently: they are not the user touching a 3/4 layer).
pub fn plan(find: impl Fn((u8, u8)) -> Option<Binding>, ev: Ev, shown: usize) -> Step {
    let on = ev.on();
    let Some(b) = find(ev.key()) else {
        // The LSB half of a 14-bit control has no binding of its own.
        let msb = ev.msb_key().and_then(find);
        return match msb {
            Some(b) if b.hires && (b.deck as usize) < shown => Step::Knob,
            Some(_) => Step::Skip("14-bit control of a hidden deck"),
            None => Step::Skip("unmapped"),
        };
    };
    let d = b.deck as usize;
    // The deck layer lights come before the layout check: a controller already on its DECK 3 / 4 layer widens a 2-deck layout.
    if let Kind::Layer = ACTIONS[b.act].kind {
        return if on {
            Step::Layer(d)
        } else {
            Step::Skip("deck not selected")
        };
    }
    if d >= shown {
        return Step::Skip("deck layer not shown");
    }
    match &ACTIONS[b.act].kind {
        Kind::Knob(_) => Step::Knob,
        Kind::Vinyl => Step::Vinyl(d, on),
        Kind::Cutter => Step::Cutter(d, on),
        _ => Step::Skip("button state, not a press"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{decode, mapfile};
    use super::*;

    /// A made-up mapping file with the control kinds a state reply reports, for each of the 4 decks.
    fn mapping() -> String {
        let mut s =
            String::from("<OpenFadeRMapping><info><name>Test Controller</name></info><controls>");
        for d in 0..4 {
            let (n, c) = (0x90 + d, 0xB0 + d);
            for (st, no, act, hires) in [
                (n, 0x0B, "play", false),
                (c, 0x13, "fader", true),
                (c, 0x00, "tempo", true),
                (c, 0x07, "eq-hi", true),
                (c, 0x04, "trim", false),
                (n, 0x3C, "hw-deck-select", false),
                (n, 0x17, "vinyl", false),
                (n, 0x0C, "cue", false),
            ] {
                s += &format!(
                    "<control status=\"{st}\" midino=\"{no}\" action=\"{act}\" deck=\"{d}\" hires=\"{hires}\"/>"
                );
            }
        }
        s + "<control status=\"0xB6\" midino=\"0x1F\" action=\"crossfader\" deck=\"0\" hires=\"true\"/>\
             </controls></OpenFadeRMapping>"
    }

    fn plans(shown: usize, msgs: &[[u8; 3]]) -> Vec<Step> {
        let (m, _) = mapfile::parse(&mapping()).unwrap();
        assert!(m.bad.is_empty(), "{:?}", m.bad);
        let find = |k| m.binds.iter().find(|b| (b.status, b.d1) == k).copied();
        msgs.iter()
            .map(|&b| plan(find, decode(b).expect("a channel message"), shown))
            .collect()
    }

    /// The shape of a controller's state reply: 14-bit pairs, then the deck layer lights and other button states.
    const REPLY: &[[u8; 3]] = &[
        [0xB0, 0x04, 0x47], // trim, 7-bit
        [0xB1, 0x13, 0x00], // channel fader MSB / LSB, deck B
        [0xB1, 0x33, 0x00],
        [0xB3, 0x07, 0x40], // EQ of deck D
        [0xB6, 0x1F, 0x32], // crossfader
        [0xB6, 0x3F, 0x32],
        [0x93, 0x0D, 0x7F], // a state we have no binding for
        [0x90, 0x17, 0x7F], // vinyl on, deck A
        [0x91, 0x17, 0x00], // vinyl off, deck B (note-on velocity 0)
        [0x90, 0x3C, 0x7F], // deck A selected on the left
        [0x92, 0x3C, 0x00], // deck C not selected
        [0x91, 0x3C, 0x7F], // deck B selected on the right
        [0x90, 0x0B, 0x7F], // play lit: not a press
        [0x90, 0x0C, 0x7F], // cue lit: not a press
    ];

    #[test]
    fn four_deck_reply_applies_controls_and_state_but_no_button() {
        use Step::*;
        assert_eq!(
            plans(4, REPLY),
            [
                Knob,
                Knob,
                Knob,
                Knob,
                Knob,
                Knob,
                Skip("unmapped"),
                Vinyl(0, true),
                Vinyl(1, false),
                Layer(0),
                Skip("deck not selected"),
                Layer(1),
                Skip("button state, not a press"),
                Skip("button state, not a press"),
            ]
        );
    }

    #[test]
    fn two_deck_layout_skips_the_hidden_decks_silently() {
        use Step::*;
        let p = plans(2, REPLY);
        assert_eq!(p[3], Skip("deck layer not shown"), "EQ of deck D");
        assert_eq!(
            p[10],
            Skip("deck not selected"),
            "a hidden deck C that is not selected"
        );
        assert_eq!(
            plans(2, &[[0x92, 0x3C, 0x7F]]),
            [Layer(2)],
            "the controller is on deck C: the layout widens"
        );
        assert_eq!(
            [p[1], p[2], p[4], p[5], p[9]],
            [Knob, Knob, Knob, Knob, Layer(0)],
            "A / B controls still apply"
        );
        // the LSB of a hidden deck's 14-bit pair
        assert_eq!(
            plans(2, &[[0xB3, 0x27, 0x00]]),
            [Skip("14-bit control of a hidden deck")]
        );
    }

    #[test]
    fn the_reply_window_waits_for_a_first_message_then_for_quiet() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = Window::new(t0);
        assert!(
            w.open(at(499)) && !w.open(at(500)),
            "no reply: gives up after FIRST"
        );
        w.saw(at(20), true);
        w.saw(at(80), false);
        assert!(
            w.open(at(229)) && !w.open(at(230)),
            "closes QUIET after the last reply"
        );
        assert_eq!((w.seen, w.applied), (2, 1));
        w.saw(at(1400), true);
        assert!(!w.open(at(1500)), "never longer than MAX");
    }
}
