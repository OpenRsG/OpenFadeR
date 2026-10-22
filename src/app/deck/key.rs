//! Key shift and the deck's effective key. Pure maths first, then the `Deck` controls.
//!
//! Effective key = library key + manual key shift (semitones, `Player::set_key_shift`) + the pitch
//! change of the tempo when key lock is off (`12·log2(rate)` rounded). Shifting keeps the mode, so
//! a semitone moves a Camelot key by 7 numbers ([`camelot::shift`]).
//!
//! KEY SYNC: shift this deck by the smallest |n|
//! ≤ 6 semitones so its key equals the other deck's effective key; a deck in the other mode can
//! only reach the other key's relative major/minor (the same Camelot number), so that is the
//! target then. The manual shift resets with each LOAD.

use super::Deck;
use crate::app::camelot::{self, Key, camelot};
use crate::app::prefs::{self, KeyNotation};

/// Largest manual shift either way (the player's own limit).
pub const MAX_ST: i32 = 12;

/// Pitch change of varispeed playback at `rate` in semitones; 0 with key lock on.
fn tempo_st(rate: f64, lock: bool) -> f64 {
    if lock { 0.0 } else { 12.0 * rate.log2() }
}

/// Whole semitones the key moves in total.
fn semitones(shift: i32, rate: f64, lock: bool) -> i32 {
    shift + tempo_st(rate, lock).round() as i32
}

/// Key as shown in `notation`: the library's key until something moves it, then the shifted key
/// (the raw text when it is not a known key).
pub fn shown(raw: &str, shift: i32, rate: f64, lock: bool, notation: KeyNotation) -> String {
    match (semitones(shift, rate, lock), camelot(raw)) {
        (0, _) | (_, None) => camelot::display(raw, notation),
        (n, Some(k)) => camelot::format(camelot::shift(k, n), notation),
    }
}

/// Total pitch change e.g. "+2.7 st" (tempo and manual shift together); empty when ~0.
pub fn shift_text(shift: i32, rate: f64, lock: bool) -> String {
    let st = shift as f64 + tempo_st(rate, lock);
    if st.abs() < 0.05 {
        String::new()
    } else {
        format!("{st:+.1} st")
    }
}

/// Manual shift in −6..=6 that takes `own` (tempo already included, no manual shift) to `target`
/// or, for the other mode, to target's relative key. `n` and `n − 12` tie at 6: +6.
pub fn sync_shift(own: Key, target: Key) -> i32 {
    let m = (7 * (target.0 as i32 - own.0 as i32)).rem_euclid(12); // 7 is its own inverse mod 12
    if m > 6 { m - 12 } else { m }
}

impl Deck {
    pub(super) fn key_lock(&self) -> bool {
        self.with_player(|p| p.key_lock()).unwrap_or(false)
    }

    /// Library key with the tempo's pitch change, no manual shift.
    fn tempo_key(&self) -> Option<Key> {
        let (rate, lock) = (self.rate.get(), self.key_lock());
        camelot(&self.loaded.borrow().key).map(|k| camelot::shift(k, semitones(0, rate, lock)))
    }

    /// Key the deck sounds in now (`None` for an unknown key).
    fn effective_key(&self) -> Option<Key> {
        self.tempo_key()
            .map(|k| camelot::shift(k, self.key_st.get()))
    }

    /// Manual shift of `st` semitones (clamped ±12), applied to the player and shown.
    pub fn set_key_shift(&self, st: i32) {
        if !self.ready() {
            return;
        }
        let st = st.clamp(-MAX_ST, MAX_ST);
        self.key_st.set(st);
        self.with_player(|p| p.set_key_shift(st as f32));
        self.refresh_key();
        self.refresh_pads();
    }

    /// KEY − / KEY +: one semitone each.
    pub fn key_step(&self, d: i32) {
        self.set_key_shift(self.key_st.get() + d);
    }

    /// Make this deck's key match the master's (else another deck's, see [`Deck::key_target`]; see the module doc).
    pub fn key_sync(&self) {
        if !self.ready() {
            return;
        }
        let Some(other) = self.key_target() else {
            return self.say(tr!("key sync: load a track on another deck first"));
        };
        let (Some(own), Some(target)) = (self.tempo_key(), other.effective_key()) else {
            return self.say(tr!("key sync: both decks need a key"));
        };
        let m = sync_shift(own, target);
        self.set_key_shift(m);
        let key = camelot::format(camelot::shift(own, m), prefs::with(|p| p.key_notation));
        let m = format!("{m:+}");
        self.say(if own.1 == target.1 {
            tr!("key sync: {} st → {}", m, key)
        } else {
            tr!("key sync: {} st → {} (relative key)", m, key)
        });
    }

    /// Refresh the key, shifted flag and shift text in the view (rate, key lock, shift or key changed).
    pub(super) fn refresh_key(&self) {
        let raw = self.loaded.borrow().key.clone();
        let (st, rate, lock) = (self.key_st.get(), self.rate.get(), self.key_lock());
        let notation = prefs::with(|p| p.key_notation);
        let (text, eff) = (
            shift_text(st, rate, lock),
            shown(&raw, st, rate, lock, notation),
        );
        self.show(|v| {
            v.key = raw.into();
            v.key_eff = eff.into();
            v.key_shifted = !text.is_empty();
            v.key_shift = text.into();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        camelot(s).unwrap()
    }

    #[test]
    fn effective_key_adds_shift_and_unlocked_tempo() {
        use KeyNotation::{Camelot, Classic};
        // +1 semitone from the fader (rate 2^(1/12)) with key lock off, plus 2 manual: 8A → +3 st
        let r = 2f64.powf(1.0 / 12.0);
        assert_eq!(shown("8A", 2, r, false, Camelot), "5A"); // Am + 3 semitones = Cm
        assert_eq!(shown("8A", 2, r, false, Classic), "Cm");
        // key lock ignores the rate
        assert_eq!(
            shown("8A", 2, r, true, Camelot),
            camelot::name(camelot::shift(k("8A"), 2))
        );
        // untouched keys show in the chosen notation; unknown keys are left alone
        assert_eq!(shown("Am", 0, 1.0, false, Camelot), "8A");
        assert_eq!(
            shown("8A", 0, 1.004, false, Classic),
            "Am",
            "a tempo change under half a semitone does not move the key"
        );
        assert_eq!(shown("?", 3, 1.0, false, Camelot), "?");
    }

    #[test]
    fn shift_text_totals_manual_and_tempo() {
        assert_eq!(shift_text(0, 1.0, false), "");
        assert_eq!(shift_text(0, 1.04, true), "");
        assert_eq!(shift_text(2, 1.0, false), "+2.0 st");
        assert_eq!(shift_text(-3, 1.0, true), "-3.0 st");
        assert_eq!(shift_text(0, 1.04, false), "+0.7 st");
        assert_eq!(shift_text(1, 2f64.powf(1.0 / 12.0), false), "+2.0 st");
        assert_eq!(shift_text(0, 0.5, false), "-12.0 st");
    }

    #[test]
    fn key_sync_takes_the_shortest_shift() {
        // Am → Em is +7 semitones = −5 the other way
        assert_eq!(sync_shift(k("Am"), k("Em")), -5);
        assert_eq!(sync_shift(k("Am"), k("Am")), 0);
        assert_eq!(sync_shift(k("Am"), k("Bm")), 2);
        // F# → C is a tritone: the tie goes to +6
        assert_eq!(sync_shift(k("F#"), k("C")), 6);
        // the result lands on the target, or on its relative key across modes
        for own in 1..=12u8 {
            for target in 1..=12u8 {
                for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
                    let m = sync_shift((own, a), (target, b));
                    assert!((-6..=6).contains(&m));
                    assert_eq!(
                        camelot::shift((own, a), m),
                        (target, a),
                        "{own}{a} → {target}{b}"
                    );
                }
            }
        }
        // C major deck against A minor (its relative): nothing to do
        assert_eq!(sync_shift(k("C"), k("Am")), 0);
        // Am deck against D major: relative minor of D is Bm = +2
        assert_eq!(sync_shift(k("Am"), k("D")), 2);
    }
}
