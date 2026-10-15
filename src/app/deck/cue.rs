//! CUE with player semantics, one state machine for mouse, keyboard and controller (`cue_press` /
//! `cue_release`). Player-style behaviour:
//! * playing: CUE returns to the cue point and pauses;
//! * paused on the cue point: CUE plays from it while held, release returns and pauses (preview);
//! * paused elsewhere: CUE sets the cue point at the (quantized) playhead and previews from it;
//! * PLAY pressed while the preview is held latches playback (release then leaves it playing).
//!
//! The cue point is chosen at LOAD ([`autocue`](super::autocue): nearest cue, first sound, first memory cue) until the user sets another (session only).

use super::{Deck, fmt_ms};

/// A paused playhead this close to the cue point counts as being on it.
const AT_CUE_MS: f64 = 30.0;

/// State of the CUE button's preview.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Preview {
    #[default]
    Off,
    /// CUE held, playing from the cue point.
    Held,
    /// PLAY pressed during the preview: keeps playing after CUE is released.
    Latched,
}

/// What a CUE press does.
#[derive(Debug, PartialEq)]
pub enum Press {
    /// Pause and go back to the cue point.
    Return,
    /// Play from the cue point; `set` moves the cue point there first.
    Preview { set: Option<u32> },
}

/// `mark_ms` is where a new cue point would go (the playhead, snapped when quantize is on).
pub fn press(playing: bool, pos_ms: f64, cue_ms: u32, mark_ms: u32) -> Press {
    if playing {
        Press::Return
    } else if at_cue(pos_ms, cue_ms) {
        Press::Preview { set: None }
    } else {
        Press::Preview { set: Some(mark_ms) }
    }
}

pub fn at_cue(pos_ms: f64, cue_ms: u32) -> bool {
    (pos_ms - cue_ms as f64).abs() <= AT_CUE_MS
}

impl Preview {
    /// PLAY pressed: a held preview becomes permanent playback.
    pub fn play(self) -> Self {
        if self == Self::Held {
            Self::Latched
        } else {
            self
        }
    }

    /// CUE released: the new state and whether to return to the cue point and pause.
    pub fn release(self) -> (Self, bool) {
        (Self::Off, self == Self::Held)
    }
}

impl Deck {
    pub fn cue_press(&self) {
        // A press while the last one is still held is key repeat.
        if !self.ready() || self.cue_state.get() != Preview::Off {
            return;
        }
        let (act, cue) = {
            let l = self.loaded.borrow();
            let (pos, playing) = self.clock(&l);
            (press(playing, pos, l.cue_ms, self.mark(&l)), l.cue_ms)
        };
        let at = match act {
            Press::Preview { set: Some(ms) } => {
                self.loaded.borrow_mut().cue_ms = ms;
                self.say(format!("cue point set at {} (session only)", fmt_ms(ms)));
                ms
            }
            _ => cue,
        };
        match act {
            Press::Return => self.pause_at(at),
            Press::Preview { .. } => {
                self.cue_state.set(Preview::Held);
                self.with_player(|p| {
                    p.seek(at);
                    p.play();
                });
            }
        }
    }

    pub fn cue_release(&self) {
        let (state, back) = self.cue_state.get().release();
        self.cue_state.set(state);
        if back {
            let cue = self.loaded.borrow().cue_ms;
            self.pause_at(cue);
        }
    }

    /// PLAY pressed: a held CUE preview becomes playback. `true` when it did (the press is consumed).
    pub(super) fn cue_latch(&self) -> bool {
        let state = self.cue_state.get();
        self.cue_state.set(state.play());
        state == Preview::Held
    }

    /// Whether the CUE button is lit: the playhead rests on the cue point, or a preview is playing.
    pub(super) fn cue_lit(&self, pos_ms: f64, playing: bool, cue_ms: u32) -> bool {
        self.cue_state.get() != Preview::Off || (!playing && at_cue(pos_ms, cue_ms))
    }

    /// Shift+CUE on the controller: playhead to the start of the track.
    pub fn jump_to_start(&self) {
        if self.ready() {
            self.with_player(|p| p.seek(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_returns_to_the_cue_point() {
        assert_eq!(press(true, 5000.0, 1000, 5000), Press::Return);
    }

    #[test]
    fn paused_on_the_cue_point_previews_it() {
        assert_eq!(
            press(false, 1010.0, 1000, 1010),
            Press::Preview { set: None }
        );
    }

    #[test]
    fn paused_elsewhere_sets_a_new_cue_point_at_the_mark() {
        assert_eq!(
            press(false, 4990.0, 1000, 5000),
            Press::Preview { set: Some(5000) }
        );
        assert_eq!(
            press(false, 1031.0, 1000, 1031),
            Press::Preview { set: Some(1031) },
            "outside the tolerance"
        );
    }

    #[test]
    fn release_returns_unless_play_latched_the_preview() {
        assert_eq!(Preview::Held.release(), (Preview::Off, true));
        assert_eq!(Preview::Held.play(), Preview::Latched);
        assert_eq!(Preview::Held.play().release(), (Preview::Off, false));
        assert_eq!(Preview::Off.release(), (Preview::Off, false));
        assert_eq!(Preview::Off.play(), Preview::Off);
    }
}
