//! A Cue Bank on a deck: loaded onto the deck, its banks A to H are the HOT CUE pads. A bank is the
//! cue or loop of any track: a pad of the loaded track calls its cue, a pad of another track loads that track, puts the
//! playhead on the cue and plays. The list stays until a track is loaded the ordinary way (LOAD, a double click, a controller)
//! or [`Deck::unload_bank`]. The browser's Cue Bank (`browser::hotcuebank`) hands the banks over and reads what a
//! bank can take from the deck ([`Deck::bank_source`]).

use super::{Deck, pad_label, pads::cue_pads, quantize::Call};
use crate::app::PadView;
use openfader::{analysis::Cue, library::Track, store};

/// The list on a deck: its name and the cue of each bank with its track, letter A first (`None`: that bank's track is not in
/// the library).
pub struct Armed {
    pub name: String,
    pub slots: Vec<Option<(Track, Cue)>>,
}

/// What a bank can take from a deck: its library track, the playhead, the active loop and the hot cues A to H.
pub struct Source {
    pub track: Track,
    pub playhead: u32,
    pub looped: Option<(u32, u32)>,
    pub cues: Vec<Cue>,
}

/// The 8 pads of a loaded list: lit for a bank that holds a cue, showing its time.
pub fn views(b: &Armed) -> Vec<PadView> {
    cue_pads(|i| b.slots.get(i).and_then(Option::as_ref).map(|(_, c)| c))
}

impl Deck {
    /// Puts list `name` on the pads (HOT CUE mode).
    pub(in crate::app) fn load_bank(&self, name: &str, slots: Vec<Option<(Track, Cue)>>) {
        let n = slots.iter().flatten().count();
        *self.bank.borrow_mut() = Some(Armed {
            name: name.to_owned(),
            slots,
        });
        self.set_pad_mode(0);
        self.say(tr!("Cue Bank “{}” on the pads ({} banks)", name, n));
    }

    /// The pads go back to the loaded track's own hot cues.
    pub(in crate::app) fn unload_bank(&self) {
        if self.bank.borrow_mut().take().is_some() {
            self.refresh_pads();
        }
    }

    /// Name of the list on the pads.
    pub(in crate::app) fn bank_name(&self) -> Option<String> {
        self.bank.borrow().as_ref().map(|b| b.name.clone())
    }

    /// The loaded library track (a sampler slice is none) with what a bank can take from it; `None` while nothing is ready.
    pub(in crate::app) fn bank_source(&self) -> Option<Source> {
        if !self.ready() {
            return None;
        }
        let l = self.loaded.borrow();
        Some(Source {
            track: l.track.clone()?,
            playhead: self.clock(&l).0 as u32,
            looped: l.loop_,
            cues: l
                .analysis
                .cues
                .iter()
                .filter(|c| (1..=8).contains(&c.hot_cue))
                .cloned()
                .collect(),
        })
    }

    /// HOT CUE pad `pad` while a list is on the pads; `false` when none is (the pad's own cue applies).
    pub(super) fn bank_pad(&self, pad: usize) -> bool {
        let slot = match self.bank.borrow().as_ref() {
            None => return false,
            Some(b) => b.slots.get(pad).cloned().flatten(),
        };
        let Some((track, cue)) = slot else {
            self.say(tr!("bank {} is empty", pad_label(pad)));
            return true;
        };
        let here = self.ready()
            && self
                .loaded
                .borrow()
                .track
                .as_ref()
                .is_some_and(|t| store::key(t) == store::key(&track));
        if here {
            self.call(call_of(&cue));
            return true;
        }
        let (armed, serial) = (self.bank.take(), self.load_serial.get());
        self.load(&track, &*self.lib); // clears the list, so it is put back below
        *self.bank.borrow_mut() = armed;
        if self.load_serial.get() != serial {
            // Not locked out (EJECT / LOAD LOCK): the new track starts on the bank's cue.
            let mut l = self.loaded.borrow_mut();
            (l.cue_ms, l.cue_locked) = (cue.time_ms, true);
            drop(l);
            self.with_player(|p| p.seek(cue.time_ms));
            self.autoplay.set(true);
        }
        self.refresh_pads();
        true
    }
}

/// A loop plays as a loop, a plain cue jumps there.
pub(super) fn call_of(c: &Cue) -> Call {
    c.loop_end_ms
        .filter(|&e| e > c.time_ms)
        .map_or(Call::Hot(c.time_ms), |e| Call::Loop(c.time_ms, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(hot: u16, ms: u32, end: Option<u32>) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: end,
            color: Some([10, 20, 30]),
            comment: None,
        }
    }

    #[test]
    fn a_loaded_list_lights_only_the_banks_it_holds() {
        let b = Armed {
            name: "x".into(),
            slots: vec![
                Some((Track::default(), cue(1, 61_500, None))),
                None, // its track left the library
                Some((Track::default(), cue(3, 5_000, Some(9_000)))),
            ],
        };
        let v = views(&b);
        assert_eq!(v.len(), 8);
        assert_eq!(
            v.iter().map(|p| p.set).collect::<Vec<_>>(),
            [true, false, true, false, false, false, false, false]
        );
        assert_eq!(v[0].time, "1:01.5");
        assert_eq!(v[2].label, "C");
    }

    #[test]
    fn a_loop_bank_calls_the_loop_a_cue_bank_jumps() {
        assert_eq!(call_of(&cue(1, 100, None)), Call::Hot(100));
        assert_eq!(call_of(&cue(1, 100, Some(900))), Call::Loop(100, 900));
        assert_eq!(
            call_of(&cue(1, 100, Some(100))),
            Call::Hot(100),
            "an empty loop is a cue"
        );
    }
}
