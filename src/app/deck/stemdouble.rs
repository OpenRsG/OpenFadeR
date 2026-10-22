//! STEM INSTANT DOUBLES (the controller rows `VocalPartInstDoubles` … `DrumsPartInstDoubles`, STEMS pads 5 to 8 on screen).
//!
//! The destination shares the source's completed audio and stems. When they attach, transport and
//! the selected part follow the source only if it still has the same load and audio identity.

use super::{DECK_NAMES, Deck};
use crate::app::prefs;
use openfader::audio::{BASS, STEM_COUNT, STEM_NAMES, Track16};
use std::{rc::Rc, sync::Arc};

pub(super) struct Pending {
    part: usize,
    source: usize,
    serial: u32,
    pcm: Arc<Track16>,
}

impl Pending {
    fn matches(&self, serial: u32, source: &Arc<Track16>, target: &Arc<Track16>) -> bool {
        serial == self.serial && Arc::ptr_eq(source, &self.pcm) && Arc::ptr_eq(target, &self.pcm)
    }
}

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
        if self.eject_locked() {
            return;
        }
        let (stems, four) = prefs::with(|p| (p.stems, p.stems_four));
        if !stems {
            return self.say(tr!("STEMS is off: turn it on in PREFS"));
        }
        if k >= STEM_COUNT || (k == BASS && !four) {
            return;
        }
        let Some(from) = self.partner() else {
            return self.say(tr!("instant doubles: no other deck in this layout"));
        };
        let src = {
            let l = from.loaded.borrow();
            l.track.clone().map(|t| (t, l.analysis.clone()))
        };
        let Some((track, analysis)) = src else {
            return self.say(tr!("instant doubles: the other deck has no library track"));
        };
        let Some(stems) = from.completed_stems() else {
            return self.say(tr!(
                "instant doubles: the other deck's stems are not finished yet"
            ));
        };
        let Some(pcm) = from.with_player(|p| p.pcm()) else {
            return;
        };
        let serial = from.load_serial.get();
        self.install_shared(
            &track,
            analysis,
            vec![tr!(
                "instant double {}",
                crate::app::i18n::tr_dyn(STEM_NAMES[k])
            )],
            pcm.clone(),
            stems,
        );
        *self.double.borrow_mut() = Some(Pending {
            part: k,
            source: from.index,
            serial,
            pcm,
        });
    }

    /// The stems of a doubled track landed: line up with the other deck and swap the part (see the module doc).
    pub(super) fn finish_double(&self) {
        let Some(pending) = self.double.borrow_mut().take() else {
            return;
        };
        let Some(from) = self.deck_at(pending.source) else {
            return;
        };
        let same = from
            .with_player(|p| p.pcm())
            .zip(self.with_player(|p| p.pcm()))
            .is_some_and(|(source, target)| {
                pending.matches(from.load_serial.get(), &source, &target)
            });
        if !same {
            return self.say(tr!("instant doubles: the other deck changed track"));
        }
        let k = pending.part;
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
        let name = crate::app::i18n::tr_dyn(STEM_NAMES[k]);
        self.say(tr!(
            "instant double {}: {} here, muted on {}",
            name,
            name,
            DECK_NAMES[from.index]
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

    #[test]
    fn a_late_double_never_mutes_a_reloaded_source_or_replaced_destination() {
        use super::super::stems::gains;
        let audio = || {
            Arc::new(Track16::from(openfader::audio::Pcm {
                rate: 44100,
                samples: vec![0.25; 64],
            }))
        };
        let shared = audio();
        let pending = Pending {
            part: VOCAL,
            source: 0,
            serial: 3,
            pcm: shared.clone(),
        };
        for (serial, source, target, expected) in [
            (3, shared.clone(), shared.clone(), [0.0, 1.0, 1.0, 1.0]),
            (4, shared.clone(), shared.clone(), [1.0; STEM_COUNT]),
            (3, audio(), shared.clone(), [1.0; STEM_COUNT]),
            (3, shared.clone(), audio(), [1.0; STEM_COUNT]),
        ] {
            let mutes = if pending.matches(serial, &source, &target) {
                doubled(pending.part, [false; STEM_COUNT]).1
            } else {
                [false; STEM_COUNT]
            };
            assert_eq!(gains(mutes, [1.0; 3], true), expected);
        }
    }
}
