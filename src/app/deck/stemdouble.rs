//! STEM INSTANT DOUBLES (the controller rows `VocalPartInstDoubles` … `DrumsPartInstDoubles`, STEMS pads 5 to 8 on screen).
//!
//! The press loads a track from the other deck to the one pressed on; during playback the position is matched, the pressed deck
//! outputs only the selected stem and that stem on the other deck is muted. So with part `k`: this deck loads the other deck's track (its session analysis included), and once the stems of that
//! track are ready (a cache hit: the other deck already separated it) it jumps to the other deck's position, takes its
//! tempo, key lock and key shift, plays when it plays, solos `k` here and mutes `k` there ([`doubled`]). "The other
//! deck" is the neighbour on the mixer, A↔B, C↔D, or the one deck
//! PREFS > Extensions > STEM Instant Doubles names ([`source`]).
//! The position is read when the stems land, not at the press, so a load that took seconds still lines up.

use super::{DECK_NAMES, Deck};
use crate::app::prefs;
use openfader::audio::{BASS, STEM_COUNT, STEM_NAMES};
use std::rc::Rc;

/// Mute states `(this deck, the other deck)` after INSTANT DOUBLES of part `k`, the other deck having `other` now:
/// here every part but `k` is muted, there `k` is muted on top of what was.
pub fn doubled(
    k: usize,
    mut other: [bool; STEM_COUNT],
) -> ([bool; STEM_COUNT], [bool; STEM_COUNT]) {
    other[k] = true;
    (std::array::from_fn(|j| j != k), other)
}

/// The deck `me` doubles: the one PREFS names (`fixed`), else the neighbour on the mixer; `None` when that is `me` itself
/// or not in the layout (`shown` decks).
pub fn source(me: usize, fixed: Option<usize>, shown: usize) -> Option<usize> {
    let d = fixed.unwrap_or(me ^ 1);
    (d != me && d < shown).then_some(d)
}

impl Deck {
    /// The deck whose track this one doubles (PREFS > Extensions > STEM Instant Doubles).
    fn partner(&self) -> Option<Rc<Deck>> {
        self.deck_at(source(
            self.index,
            prefs::with(|p| p.extensions.doubles_deck()),
            super::active(),
        )?)
    }

    /// INSTANT DOUBLES of part `k` (0 VOCAL, 1 INST, 2 BASS, 3 DRUMS) from the other deck. Says why when it cannot.
    pub fn stem_double(&self, k: usize) {
        let (stems, four) = prefs::with(|p| (p.stems, p.stems_four));
        if !stems {
            return self.say("STEMS is off: turn it on in PREFS");
        }
        if k >= STEM_COUNT || (k == BASS && !four) {
            return;
        }
        let Some(from) = self.partner() else {
            return self.say("instant doubles: no other deck in this layout");
        };
        let src = {
            let l = from.loaded.borrow();
            l.track.clone().map(|t| (t, l.analysis.clone()))
        };
        let Some((track, analysis)) = src else {
            return self.say("instant doubles: the other deck has no library track");
        };
        if !from.stems_complete() {
            return self.say("instant doubles: the other deck's stems are not finished yet");
        }
        self.install(
            &track,
            analysis,
            vec![format!("instant double {}", STEM_NAMES[k])],
            None,
        );
        self.double.set(Some(k)); // after `install`: loading clears a pending double
    }

    /// The stems of a doubled track landed: line up with the other deck and swap the part (see the module doc).
    pub(super) fn finish_double(&self) {
        let Some(k) = self.double.take() else { return };
        let Some(from) = self.partner() else { return };
        let same = from.loaded.borrow().track.as_ref().map(|t| &t.file)
            == self.loaded.borrow().track.as_ref().map(|t| &t.file);
        if !same {
            return self.say("instant doubles: the other deck changed track");
        }
        let (pos, playing, key_lock) = from
            .with_player(|p| (p.position_ms(), p.is_playing(), p.key_lock()))
            .unwrap_or_default();
        self.set_rate(from.rate());
        self.with_player(|p| p.set_key_lock(key_lock));
        self.show(|v| v.key_lock = key_lock);
        self.set_key_shift(from.key_st.get());
        let (here, there) = doubled(k, from.stem_mutes());
        self.set_stem_mutes(here);
        from.set_stem_mutes(there);
        self.with_player(|p| {
            p.seek(pos.round() as u32);
            if playing {
                p.play();
            }
        });
        self.say(format!(
            "instant double {}: {} here, muted on {}",
            STEM_NAMES[k], STEM_NAMES[k], DECK_NAMES[from.index]
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::audio::{BASS, DRUMS, INST, VOCAL};

    #[test]
    fn the_source_deck_is_the_neighbour_or_the_one_in_prefs() {
        assert_eq!(
            [
                source(0, None, 2),
                source(1, None, 2),
                source(2, None, 4),
                source(3, None, 4)
            ],
            [Some(1), Some(0), Some(3), Some(2)]
        );
        assert_eq!(source(0, Some(2), 4), Some(2));
        assert_eq!(source(1, Some(1), 4), None, "a deck cannot double itself");
        assert_eq!(source(0, Some(3), 2), None, "a deck outside the layout");
        assert_eq!(source(2, None, 2), None, "the neighbour of a parked deck");
    }

    #[test]
    fn the_pressed_deck_keeps_only_the_part_and_the_other_deck_loses_it() {
        let (here, there) = doubled(DRUMS, [false; STEM_COUNT]);
        assert_eq!(here, [true, true, true, false]);
        assert_eq!(there, [false, false, false, true]);
        // the other deck's earlier mutes stay
        let (_, there) = doubled(VOCAL, [false, true, false, false]);
        assert_eq!(there, [true, true, false, false]);
        // 3 stems: INST carries bass (gains() makes bass follow INST), so muting INST there silences both
        let (here, there) = doubled(INST, [false; STEM_COUNT]);
        assert!(!here[INST] && here[VOCAL] && here[DRUMS] && there[INST]);
    }

    #[test]
    fn the_swap_is_audible_as_gains_the_pressed_deck_plays_only_the_part_and_the_other_lacks_it() {
        use super::super::stems::gains;
        let (here, there) = doubled(VOCAL, [false; STEM_COUNT]);
        assert_eq!(gains(here, [1.0; 3], true), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(gains(there, [1.0; 3], true), [0.0, 1.0, 1.0, 1.0]);
        let g = gains(doubled(INST, [false; STEM_COUNT]).1, [1.0; 3], false);
        assert_eq!(
            (g[INST], g[BASS]),
            (0.0, 0.0),
            "3 stems: bass leaves with INST"
        );
    }
}
