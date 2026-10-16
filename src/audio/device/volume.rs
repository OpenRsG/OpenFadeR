//! Volume and mute of the device OpenFadeR plays on. Two backends, chosen by what the engine opens:
//!
//! * [`Target::Alsa`]: a physical card. The engine opens it as an ALSA PCM (`front:CARD=…`, `sysdefault:CARD=…`,
//!   `hdmi:CARD=…`), which goes to the hardware and bypasses every sound-server volume, so the card's own **ALSA mixer**
//!   (the `alsa` crate: `snd_mixer_selem_*`, already in the tree through cpal) is the control that changes what we play
//!   ([`super::mixer`], Linux only; on Windows and macOS no card has a mixer here and the page shows no slider, see
//!   `prefs::audio::cards`). The control is the first of its `PREFERRED` list with a playback volume, else any; "Silence" is its minimum step. [observed]
//!   the A50's USB mixer and the HDA cards expose a dB scale; an HDMI card has no volume control (the monitor owns it),
//!   which [`read`] reports as `None`, and the page then says so instead of showing a dead slider.
//! * [`Target::Sink`]: the system default and the sound-server PCMs (`pipewire`, `pulse`, `default`). They play on the
//!   server's default sink, whose volume `pactl set-sink-volume @DEFAULT_SINK@` sets: the same knob the user's own sound
//!   manager moves. (The ALSA mixer would not reach a sink that is software-mixed, e.g. a Bluetooth headset.)
//!
//! The scale is PulseAudio's / PipeWire's, so the numbers read like the user's manager: percent is the cube root of the
//! amplitude, `dB = 60·log10(percent/100)` (measured: 20% = -41.94 dB on a USB headset sink). 100% is 0 dB, or the control's maximum
//! when that is lower (PipeWire's base volume); the control's minimum is reached at the percent whose dB it
//! equals and everything below it (down to 0%, "Silence") is that minimum. A control without a dB scale maps percent to its
//! raw steps linearly.

use super::sys;

#[cfg(target_os = "linux")]
use super::mixer;

/// Windows and macOS: no ALSA, so no card has a mixer and every read is `None`.
#[cfg(not(target_os = "linux"))]
mod mixer {
    use super::Level;

    pub fn read(_: &str) -> Option<Level> {
        None
    }

    pub fn set_percent(_: &str, _: u32) -> Option<Level> {
        None
    }

    pub fn set_muted(_: &str, _: bool) -> Option<Level> {
        None
    }
}

/// What a volume control acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// The mixer of the ALSA card with this id.
    Alsa(String),
    /// The sound server's default sink.
    Sink,
}

/// A volume control as last read.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    /// 0..=100, PipeWire's cubic percent.
    pub percent: u32,
    /// The dB at `percent`, when the control has a dB scale.
    pub db: Option<f32>,
    /// The dB at 100%.
    pub top_db: Option<f32>,
    pub muted: bool,
    /// The control has a mute switch.
    pub can_mute: bool,
}

fn db_text(db: f32) -> String {
    if db.abs() < 0.005 {
        "0 dB".to_owned()
    } else {
        format!("{db:.2} dB")
    }
}

impl Level {
    /// "20% (-41.94 dB)", "Silence" at 0%.
    pub fn text(&self) -> String {
        match (self.percent, self.db) {
            (0, _) => "Silence".to_owned(),
            (p, Some(d)) => format!("{p}% ({})", db_text(d)),
            (p, None) => format!("{p}%"),
        }
    }

    /// The right-hand mark of the slider: "100% (0 dB)".
    pub fn max_text(&self) -> String {
        self.top_db
            .map_or_else(|| "100%".to_owned(), |d| format!("100% ({})", db_text(d)))
    }
}

/// dB of a PipeWire percent (-inf at 0).
pub fn db_of(percent: f32) -> f32 {
    60.0 * (percent / 100.0).log10()
}

/// PipeWire percent of a dB value relative to 100%.
pub fn percent_of(db: f32) -> f32 {
    100.0 * 10f32.powf(db / 60.0)
}

fn sink_level() -> Option<Level> {
    let name = sys::default_sink()?;
    let s = sys::sinks().into_iter().find(|s| s.name == name)?;
    Some(Level {
        percent: s.percent?,
        db: s.db.filter(|d| d.is_finite()),
        top_db: Some(0.0),
        muted: s.muted,
        can_mute: true,
    })
}

/// The current level; `None` = the device has no volume control (or its mixer / the sound server is unreachable).
pub fn read(t: &Target) -> Option<Level> {
    match t {
        Target::Alsa(card) => mixer::read(card),
        Target::Sink => sink_level(),
    }
}

/// Sets the volume to `percent` (0..=100) and returns the level read back (the sink: computed, `cur` carries the mute).
pub fn set_percent(t: &Target, percent: u32, cur: &Level) -> Option<Level> {
    let percent = percent.min(100);
    match t {
        Target::Alsa(card) => mixer::set_percent(card, percent),
        Target::Sink => {
            sys::pactl(&["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")])?;
            Some(Level {
                percent,
                db: (percent > 0).then(|| db_of(percent as f32)),
                ..cur.clone()
            })
        }
    }
}

/// Mutes or unmutes; returns the new level.
pub fn set_muted(t: &Target, muted: bool, cur: &Level) -> Option<Level> {
    match t {
        Target::Alsa(card) => mixer::set_muted(card, muted),
        Target::Sink => {
            sys::pactl(&[
                "set-sink-mute",
                "@DEFAULT_SINK@",
                if muted { "1" } else { "0" },
            ])?;
            Some(Level {
                muted,
                ..cur.clone()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_is_pipewires_cubic_percent() {
        // Measured on a USB headset sink: 20% = -41.94 dB.
        assert!((db_of(20.0) - -41.94).abs() < 0.01);
        assert_eq!(db_of(100.0), 0.0);
        assert!(db_of(0.0).is_infinite());
        assert!((percent_of(-41.94) - 20.0).abs() < 0.01);
    }

    #[test]
    fn the_label_reads_like_the_sound_manager() {
        let l = |percent, db| Level {
            percent,
            db,
            top_db: Some(0.0),
            muted: false,
            can_mute: true,
        };
        assert_eq!(l(20, Some(-41.94)).text(), "20% (-41.94 dB)");
        assert_eq!(l(100, Some(0.0)).text(), "100% (0 dB)");
        assert_eq!(l(0, Some(-65.0)).text(), "Silence");
        assert_eq!(l(40, None).text(), "40%");
        assert_eq!(l(1, None).max_text(), "100% (0 dB)");
        assert_eq!(
            Level {
                top_db: None,
                ..l(1, None)
            }
            .max_text(),
            "100%"
        );
    }
}
