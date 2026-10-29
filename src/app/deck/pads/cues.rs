//! Hot and memory cue editing from the pads (and the controller's MEMORY / delete buttons).

use super::{Loaded, hot};
use crate::app::{
    deck::{Deck, cuelist, fmt_ms, pad_label, quantize::Call},
    prefs,
};
use openfader::analysis::{COLORFUL, Cue};
use slint::{ModelRc, VecModel};
use std::cell::RefMut;

/// Sets hot cue `pad` at `ms` in the pad's colour; `false` (nothing changed) if it is already set.
fn set_hot(cues: &mut Vec<Cue>, pad: usize, ms: u32) -> bool {
    let n = pad as u16 + 1;
    if cues.iter().any(|c| c.hot_cue == n) {
        return false;
    }
    cues.push(Cue {
        hot_cue: n,
        time_ms: ms,
        loop_end_ms: None,
        color: Some(COLORFUL[pad]),
        comment: None,
    });
    true
}

/// Deletes hot cue `pad`; `false` if it was not set.
fn delete_hot(cues: &mut Vec<Cue>, pad: usize) -> bool {
    let n = cues.len();
    cues.retain(|c| c.hot_cue as usize != pad + 1);
    cues.len() != n
}

/// Adds a memory cue at `ms`; `false` if there is one there already.
fn add_memory(cues: &mut Vec<Cue>, ms: u32) -> bool {
    if cues
        .iter()
        .any(|c| c.hot_cue == 0 && c.time_ms == ms && c.loop_end_ms.is_none())
    {
        return false;
    }
    cues.push(Cue {
        hot_cue: 0,
        time_ms: ms,
        loop_end_ms: None,
        color: None,
        comment: None,
    });
    true
}

/// What pad `pad`'s hot cue does: a loop plays as a loop, a plain cue jumps there.
fn hot_call(a: &openfader::analysis::Analysis, pad: usize) -> Option<Call> {
    hot(a, pad).map(crate::app::deck::bank::call_of)
}

impl Deck {
    /// Hot-cue mode: in delete mode it deletes a set pad; otherwise a set pad jumps to its cue (or activates its loop) and plays
    /// (kept on the beat while playing with Q on, see `quantize`; paused with the HOT CUE gate setting it
    /// plays only while the pad is held, [`Deck::pad_released`]), an empty pad sets a cue at the (quantized) playhead.
    pub(super) fn hot_pad(&self, pad: usize) {
        crate::app::mixer::note(
            format!("deck{}.hotcue", self.index + 1),
            pad_label(pad).to_string(),
        );
        let mut l = self.loaded.borrow_mut();
        if self.view.borrow().delete_mode {
            if delete_hot(&mut l.analysis.cues, pad) {
                self.cues_changed(l, tr!("hot cue {} deleted", pad_label(pad)));
            }
        } else if let Some(call) = hot_call(&l.analysis, pad) {
            drop(l);
            let paused = !self.playing();
            let gated = self.gate.take().is_some();
            if let Call::Hot(ms) = call {
                if (paused || gated) && prefs::with(|p| p.controller.hot_cue_gate) {
                    self.gate.set(Some(ms));
                }
            }
            self.pad_owner.set(Some(pad));
            self.call(call);
        } else {
            let ms = self.mark(&l);
            set_hot(&mut l.analysis.cues, pad, ms);
            self.cues_changed(l, tr!("hot cue {} set at {}", pad_label(pad), fmt_ms(ms)));
        }
    }

    pub fn memory_cue(&self) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let ms = self.mark(&l);
        if add_memory(&mut l.analysis.cues, ms) {
            self.cues_changed(l, tr!("memory cue set at {}", fmt_ms(ms)));
        }
    }

    /// Shift + MEMORY on a controller: deletes the memory cue (or loop) under the playhead.
    pub fn memory_delete(&self) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let (pos, _) = self.clock(&l);
        if cuelist::delete_memory(&mut l.analysis.cues, pos) {
            self.cues_changed(l, tr!("memory cue deleted").into());
        } else {
            drop(l);
            self.say(tr!("no memory cue at the playhead"));
        }
    }

    /// Delete mode: the next pad press deletes instead of playing / setting.
    pub fn toggle_delete(&self) {
        self.show(|v| v.delete_mode = !v.delete_mode);
    }

    /// Shift + hot-cue pad on a controller: deletes hot cue `pad` whatever the mode.
    pub fn pad_delete(&self, pad: usize) {
        if !self.ready() || pad >= 8 {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        if delete_hot(&mut l.analysis.cues, pad) {
            self.cues_changed(l, tr!("hot cue {} deleted", pad_label(pad)));
        }
    }

    /// Cues changed: refresh the pads, redraw both waveforms, say what happened.
    pub(in crate::app::deck) fn cues_changed(&self, mut l: RefMut<Loaded>, mut status: String) {
        if let Err(e) = self.persist_cues(&l) {
            status += &tr!(" · NOT saved: {}", e);
        }
        l.invalidate();
        let (pads, cues) = (self.pad_views(&l), cuelist::views(&l.analysis));
        drop(l);
        self.show(|v| {
            v.pads = ModelRc::new(VecModel::from(pads));
            v.cues = ModelRc::new(VecModel::from(cues));
            v.status = status.into();
        });
        self.reread_siblings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::Analysis;

    fn cue(hot: u16, ms: u32) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    fn hot_ms(cues: &[Cue], pad: usize) -> Option<u32> {
        cues.iter()
            .find(|c| c.hot_cue as usize == pad + 1)
            .map(|c| c.time_ms)
    }

    #[test]
    fn memory_delete_takes_the_cue_under_the_playhead_only() {
        let mut c = vec![cue(0, 1000), cue(0, 5000), cue(1, 1010)];
        assert!(!cuelist::delete_memory(&mut c, 3000.0));
        assert!(cuelist::delete_memory(&mut c, 1020.0));
        assert_eq!(
            c.iter().map(|c| c.time_ms).collect::<Vec<_>>(),
            [5000, 1010]
        ); // the hot cue stays
    }

    #[test]
    fn set_hot_fills_an_empty_pad_once_and_delete_frees_it() {
        let mut cues = vec![cue(0, 5), cue(1, 100)];
        assert!(set_hot(&mut cues, 2, 777));
        assert_eq!(cues.last().and_then(|c| c.color), Some(COLORFUL[2]));
        assert!(!set_hot(&mut cues, 2, 999) && !set_hot(&mut cues, 0, 999)); // occupied: untouched
        assert_eq!([hot_ms(&cues, 2), hot_ms(&cues, 0)], [Some(777), Some(100)]);
        assert!(delete_hot(&mut cues, 2));
        assert!(!delete_hot(&mut cues, 2));
        assert_eq!(cues.len(), 2); // the memory cue and the other pad survive
        assert!(set_hot(&mut cues, 2, 5)); // a freed pad can be set again
    }

    #[test]
    fn memory_cues_do_not_take_pads_and_are_not_duplicated() {
        let mut a = Analysis::default();
        assert!(add_memory(&mut a.cues, 1200));
        assert!(!add_memory(&mut a.cues, 1200));
        assert!(add_memory(&mut a.cues, 3000));
        assert_eq!(a.cues.len(), 2);
        assert!((0..8).all(|p| hot(&a, p).is_none()));
        // a hot cue at the same time is a different cue
        assert!(set_hot(&mut a.cues, 0, 1200) && add_memory(&mut a.cues, 4000));
    }

    #[test]
    fn a_hot_cue_with_a_loop_end_is_a_loop_on_its_pad() {
        let mut a = Analysis::default();
        assert!(set_hot(&mut a.cues, 0, 1000) && set_hot(&mut a.cues, 1, 2000));
        a.cues[1].loop_end_ms = Some(4000);
        assert_eq!(
            [0, 1, 2].map(|pad| hot_call(&a, pad)),
            [Some(Call::Hot(1000)), Some(Call::Loop(2000, 4000)), None]
        );
    }
}
