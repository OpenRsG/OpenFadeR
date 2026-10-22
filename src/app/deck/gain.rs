//! AUTO GAIN of the loaded track: the analysed gain plus the user's adjustment, both kept in the store ([`store::gain`]) so a
//! reload neither re-measures nor forgets the knob. The mixer adds
//! [`Deck::auto_gain_db`] to the channel's trim while Preferences > Controller > Mixer > Auto Gain is on.

use super::Deck;
use openfader::{
    audio::{Frames, loudness},
    library::Track,
    store,
};

/// `(analysed, adjust)` in dB for a track just decoded: the analysed value from the store, else measured from `audio` (`rate`
/// Hz) and stored (the adjustment is untouched). A sampler slice (no track) is measured every time and has no adjustment.
pub(super) fn resolve<S: Frames + ?Sized>(
    track: Option<&Track>,
    audio: &S,
    rate: u32,
) -> (f32, f32) {
    let Some(key) = track.map(store::key) else {
        return (loudness::auto_gain_db(audio, rate), 0.0);
    };
    let st = store::global();
    let stored = st.gain(&key).ok().flatten().unwrap_or_default();
    let analysed = stored.analysed_db.unwrap_or_else(|| {
        let db = loudness::auto_gain_db(audio, rate);
        if let Err(e) = st.set_gain_analysed(&key, db) {
            eprintln!("deck: AUTO GAIN not saved: {e}");
        }
        db
    });
    (analysed, stored.adjust_db)
}

pub(super) fn stored(track: Option<&Track>) -> (f32, f32) {
    let gain = track
        .and_then(|t| store::global().gain(&store::key(t)).ok().flatten())
        .unwrap_or_default();
    (gain.analysed_db.unwrap_or(0.0), gain.adjust_db)
}

impl Deck {
    /// AUTO GAIN hook for the mixer: the gain in dB that evens out this deck's track, analysed + adjustment within +-12 dB
    /// (0 when nothing is loaded).
    pub(in crate::app) fn auto_gain_db(&self) -> f32 {
        let l = self.loaded.borrow();
        store::effective_db(l.gain_analysed, l.gain_adjust)
    }

    /// The user's offset on the analysed gain in dB.
    pub(in crate::app) fn gain_adjust_db(&self) -> f32 {
        self.loaded.borrow().gain_adjust
    }

    /// The AUTO GAIN knob: sets the offset (within +-12 dB, 0.1 dB steps) and saves it for the track.
    pub(in crate::app) fn set_gain_adjust(&self, db: f32) {
        let db = (db.clamp(-loudness::MAX_GAIN_DB, loudness::MAX_GAIN_DB) * 10.0).round() / 10.0;
        let key = {
            let mut l = self.loaded.borrow_mut();
            if l.gain_adjust == db {
                return;
            }
            l.gain_adjust = db;
            l.track.as_ref().map(store::key)
        };
        if let Some(Err(e)) = key.map(|k| store::global().set_gain_adjust(&k, db)) {
            self.say(tr!("AUTO GAIN not saved: {}", e));
        }
    }
}
