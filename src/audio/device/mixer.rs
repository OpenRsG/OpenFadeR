//! The ALSA mixer of one card (Linux only): the volume and mute [`super::volume`] reads and sets for [`super::volume::Target::Alsa`].
//! The control is the first of [`PREFERRED`] with a playback volume, else any; [`super::volume`] explains the scale.

use super::volume::{db_of, percent_of, Level};
use alsa::mixer::{MilliBel, Mixer, Selem, SelemChannelId, SelemId};
use alsa::Round;

/// ALSA controls tried in this order for the playback volume.
const PREFERRED: [&str; 7] = [
    "Master",
    "PCM",
    "Speaker",
    "Headphone",
    "Front",
    "Line Out",
    "Headset",
];

/// The dB range of one ALSA control and the percent <-> dB mapping over it.
struct Scale {
    min: f32,
    max: f32,
}

impl Scale {
    /// The dB of 100%.
    fn top(&self) -> f32 {
        self.max.min(0.0)
    }

    fn db(&self, percent: u32) -> f32 {
        (self.top() + db_of(percent as f32))
            .max(self.min)
            .min(self.top())
    }

    fn percent(&self, db: f32) -> u32 {
        percent_of(db - self.top()).round().clamp(0.0, 100.0) as u32
    }
}

const FRONT: SelemChannelId = SelemChannelId::FrontLeft;

/// The card's playback control (see [`PREFERRED`]) handed to `f`; `None` when the card has no mixer or no such control.
fn with_selem<R>(card: &str, f: impl FnOnce(&Selem) -> Option<R>) -> Option<R> {
    let mixer = Mixer::new(&format!("hw:{card}"), false).ok()?;
    let names: Vec<String> = mixer
        .iter()
        .filter_map(Selem::new)
        .filter(|s| s.has_playback_volume())
        .filter_map(|s| s.get_id().get_name().ok().map(str::to_owned))
        .collect();
    let name = PREFERRED
        .iter()
        .find(|p| names.iter().any(|n| n == *p))
        .map(|p| (*p).to_owned())
        .or_else(|| names.first().cloned())?;
    f(&mixer.find_selem(&SelemId::new(&name, 0))?)
}

/// The scale of `sel` when it has a usable dB scale.
fn scale(sel: &Selem) -> Option<Scale> {
    let (lo, hi) = sel.get_playback_db_range();
    (hi > lo && sel.get_playback_vol_db(FRONT).is_ok()).then(|| Scale {
        min: lo.to_db(),
        max: hi.to_db(),
    })
}

fn level_of(sel: &Selem) -> Option<Level> {
    let can_mute = sel.has_playback_switch();
    let muted = can_mute && sel.get_playback_switch(FRONT).ok() == Some(0);
    let (percent, db, top_db) = match scale(sel) {
        Some(s) => {
            let db = sel.get_playback_vol_db(FRONT).ok()?.to_db();
            (s.percent(db), Some(db), Some(s.top()))
        }
        None => {
            let (lo, hi) = sel.get_playback_volume_range();
            let v = sel.get_playback_volume(FRONT).ok()?;
            let percent = ((v - lo) * 100 + (hi - lo) / 2) / (hi - lo).max(1);
            (percent.clamp(0, 100) as u32, None, None)
        }
    };
    Some(Level {
        percent,
        db,
        top_db,
        muted,
        can_mute,
    })
}

fn set_percent_of(sel: &Selem, percent: u32) -> Option<()> {
    match scale(sel) {
        Some(s) => sel
            .set_playback_db_all(MilliBel::from_db(s.db(percent)), Round::Ceil)
            .ok(),
        None => {
            let (lo, hi) = sel.get_playback_volume_range();
            sel.set_playback_volume_all(lo + ((hi - lo) * percent as i64 + 50) / 100)
                .ok()
        }
    }
}

/// The level of the card's control.
pub(super) fn read(card: &str) -> Option<Level> {
    with_selem(card, level_of)
}

/// Sets the card's control to `percent` and returns the level read back.
pub(super) fn set_percent(card: &str, percent: u32) -> Option<Level> {
    with_selem(card, |sel| {
        set_percent_of(sel, percent)?;
        level_of(sel)
    })
}

/// Mutes or unmutes the card's control; returns the level read back.
pub(super) fn set_muted(card: &str, muted: bool) -> Option<Level> {
    with_selem(card, |sel| {
        sel.set_playback_switch_all(i32::from(!muted)).ok()?;
        level_of(sel)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_reaches_its_minimum_before_zero_and_never_exceeds_100() {
        // A control from -65 dB to +12 dB: 100% is 0 dB, below ~8% it sits at the minimum.
        let s = Scale {
            min: -65.0,
            max: 12.0,
        };
        assert_eq!(s.db(100), 0.0);
        assert!((s.db(50) - -18.06).abs() < 0.01);
        assert_eq!((s.db(0), s.db(5)), (-65.0, -65.0));
        assert_eq!(
            (s.percent(0.0), s.percent(12.0), s.percent(-65.0)),
            (100, 100, 8)
        );
        assert_eq!(s.percent(s.db(37)), 37);
        // A control that tops out below 0 dB: its maximum is 100%.
        let q = Scale {
            min: -40.0,
            max: -10.0,
        };
        assert_eq!(
            (q.db(100), q.percent(-10.0), q.percent(-40.0)),
            (-10.0, 100, 32)
        );
    }
}
