//! AUTO GAIN per track: what the analysis measured plus the user's own adjustment.
//!
//! AUTO GAIN loads the attenuation amount set when the track was analysed and can be adjusted manually; the
//! settings are saved per track. The analysed dB and the manual
//! offset are kept in two columns and `analysed + adjust` is applied (within +-[`MAX_GAIN_DB`]), so a re-analysis replaces only the first.

use super::{R, Store, query_opt};
use crate::audio::loudness::MAX_GAIN_DB;
use rusqlite::params;

/// What [`Store::gain`] returns for one track.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackGain {
    /// The gain the analysis measured; `None` while only an adjustment is stored (the track is not analysed yet).
    pub analysed_db: Option<f32>,
    /// The user's offset on top of it (0 = none).
    pub adjust_db: f32,
}

impl TrackGain {
    pub fn effective_db(&self) -> f32 {
        effective_db(self.analysed_db.unwrap_or(0.0), self.adjust_db)
    }
}

/// The gain a track is played with: `analysed + adjust`, within +-[`MAX_GAIN_DB`]; 0 when either is not a number.
pub fn effective_db(analysed_db: f32, adjust_db: f32) -> f32 {
    let sum = analysed_db + adjust_db;
    if sum.is_finite() {
        sum.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    } else {
        0.0
    }
}

impl Store {
    pub fn gain(&self, key: &str) -> R<Option<TrackGain>> {
        query_opt(
            &self.db(),
            "SELECT analysed_db, adjust_db FROM gain WHERE key = ?1",
            [key],
            |r| {
                Ok(TrackGain {
                    analysed_db: r.get::<_, Option<f64>>(0)?.map(|v| v as f32),
                    adjust_db: r.get::<_, f64>(1)? as f32,
                })
            },
        )
    }

    /// Stores what the analysis measured; the user's adjustment stays.
    pub fn set_gain_analysed(&self, key: &str, db: f32) -> R<()> {
        self.run(
            "INSERT INTO gain(key, analysed_db) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET analysed_db = ?2",
            params![key, f64::from(db)],
        )
    }

    /// Stores the user's offset (0 = none); the analysed value stays.
    pub fn set_gain_adjust(&self, key: &str, db: f32) -> R<()> {
        self.run(
            "INSERT INTO gain(key, adjust_db) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET adjust_db = ?2",
            params![key, f64::from(db)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_gain_is_the_sum_within_the_limit() {
        assert_eq!(effective_db(3.0, -1.5), 1.5);
        assert_eq!(effective_db(10.0, 5.0), MAX_GAIN_DB);
        assert_eq!(effective_db(-10.0, -5.0), -MAX_GAIN_DB);
        assert_eq!(effective_db(f32::NAN, 2.0), 0.0);
    }

    #[test]
    fn analysed_and_adjust_are_kept_apart() {
        let st = Store::in_memory().unwrap();
        assert_eq!(st.gain("a|1").unwrap(), None);
        st.set_gain_analysed("a|1", -4.0).unwrap();
        st.set_gain_adjust("a|1", 2.5).unwrap();
        let g = st.gain("a|1").unwrap().unwrap();
        assert_eq!((g.analysed_db, g.adjust_db), (Some(-4.0), 2.5));
        assert_eq!(g.effective_db(), -1.5);
        // a re-analysis keeps the adjustment, a reset keeps the analysis
        st.set_gain_analysed("a|1", -6.0).unwrap();
        assert_eq!(st.gain("a|1").unwrap().unwrap().adjust_db, 2.5);
        st.set_gain_adjust("a|1", 0.0).unwrap();
        assert_eq!(st.gain("a|1").unwrap().unwrap().analysed_db, Some(-6.0));
        // an adjustment before any analysis leaves the analysis unknown
        st.set_gain_adjust("b|2", 1.0).unwrap();
        assert_eq!(st.gain("b|2").unwrap().unwrap().analysed_db, None);
    }
}
