//! Knob position (0..1, as drawn by the `Knob` widget) ↔ audio value. Every knob has its centre
//! detent at 0.5 (trim 0 dB, EQ 0 dB, CFX off) and snaps there within [`DETENT`].
//! Ranges: (trim −∞..+12 dB, EQ −26..+6 dB, CFX −1..1); the EQ ends are the player's own
//! [`EQ_KILL_DB`] / [`EQ_MAX_DB`]: a knob at 0 is a true kill for the ISOLATOR EQ type, half a dB short of it for EQ.

use crate::app::prefs::controller::EqType;
use openfader::audio::{EQ_KILL_DB, EQ_MAX_DB, cfx::Kind, loudness::MAX_GAIN_DB};

/// Knobs per channel.
pub const KNOBS: usize = 5;
/// Knob order in `ChannelView.knobs` (= the order the strip draws them).
pub const TRIM: usize = 0;
pub const HI: usize = 1;
pub const MID: usize = 2;
pub const LOW: usize = 3;
pub const CFX: usize = 4;

/// Master / headphone knobs (`BusView.knobs`): MASTER level, headphone MIX (0 = CUE, 1 = MASTER), headphone LEVEL.
pub const BUS_KNOBS: usize = 3;
pub const MASTER: usize = 0;
pub const HP_MIX: usize = 1;
pub const HP_LEVEL: usize = 2;
/// Start-up positions: master and headphone level at 0 dB, mix fully on CUE (so SPLIT puts the cue alone on the left).
pub const BUS_START: [f32; BUS_KNOBS] = [CENTRE, 0.0, CENTRE];

pub const CENTRE: f32 = 0.5;
const DETENT: f32 = 0.02;
const TRIM_FLOOR_DB: f32 = -60.0;
const TRIM_MAX_DB: f32 = 12.0;
/// Master and headphone level top (the audio side clamps at [`openfader::audio::MAX_LEVEL`], +6.02 dB).
const BUS_MAX_DB: f32 = 6.0;
/// How far above [`EQ_KILL_DB`] the bottom of an [`EqType::Eq`] band stays.
const EQ_MARGIN_DB: f32 = 0.5;

/// Clamps to 0..1 and snaps to the centre detent.
pub fn snap(k: f32) -> f32 {
    let k = if k.is_nan() {
        CENTRE
    } else {
        k.clamp(0.0, 1.0)
    };
    if (k - CENTRE).abs() < DETENT {
        CENTRE
    } else {
        k
    }
}

/// Level in dB: 0 is −∞ (silence), then −60 dB at the first step up to 0 dB at the centre, `max_db` at the top.
fn sweep_db(k: f32, max_db: f32) -> f32 {
    match k {
        k if k <= 0.0 => f32::NEG_INFINITY,
        k if k < CENTRE => (k - CENTRE) * (-TRIM_FLOOR_DB / CENTRE),
        k => (k - CENTRE) * (max_db / (1.0 - CENTRE)),
    }
}

/// Trim in dB: −∞ .. 0 dB at the centre .. +12.
pub fn trim_db(k: f32) -> f32 {
    sweep_db(k, TRIM_MAX_DB)
}

/// MASTER / headphone LEVEL in dB: −∞ .. 0 dB at the centre .. +6.
pub fn bus_db(k: f32) -> f32 {
    sweep_db(k, BUS_MAX_DB)
}

/// EQ band gain in dB: −26 .. 0 at the centre .. +6. The bottom is a true kill for [`EqType::Isolator`]; for
/// [`EqType::Eq`] it stops [`EQ_MARGIN_DB`] above the kill (the band stays faintly audible).
pub fn eq_db(k: f32, eq: EqType) -> f32 {
    let bottom = match eq {
        EqType::Isolator => EQ_KILL_DB,
        EqType::Eq => EQ_KILL_DB + EQ_MARGIN_DB,
    };
    if k <= CENTRE {
        (k - CENTRE) * (-bottom / CENTRE)
    } else {
        (k - CENTRE) * (EQ_MAX_DB / (1.0 - CENTRE))
    }
}

/// CFX sweep −1 (full left) .. 0 (off) .. +1 (full right); for FILTER −1 is the full low-pass, +1 the full high-pass.
pub fn cfx(k: f32) -> f32 {
    (k * 2.0 - 1.0).clamp(-1.0, 1.0)
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0) // −∞ → 0
}

fn db_text(db: f32) -> String {
    if db == f32::NEG_INFINITY {
        "-inf".into()
    } else {
        format!("{db:+.1} dB")
    }
}

/// AUTO GAIN knob: position 0.5 = no adjustment, the ends are -/+[`MAX_GAIN_DB`], linear in between.
pub fn auto_adjust_db(k: f32) -> f32 {
    (snap(k) - CENTRE) * 2.0 * MAX_GAIN_DB
}

/// The knob position of an adjustment ([`auto_adjust_db`] backwards).
pub fn auto_knob(adjust_db: f32) -> f32 {
    adjust_db / (2.0 * MAX_GAIN_DB) + CENTRE
}

/// Text under the AUTO GAIN knob: the gain the deck plays with (analysed + adjustment).
pub fn auto_readout(effective_db: f32) -> String {
    db_text(effective_db)
}

/// Text under a knob; `kind` is the CFX type, which names the sides of the CFX knob, `eq` the EQ type
/// (ISOLATOR shows KILL at the bottom, EQ always the dB value).
pub fn readout(kind: Kind, eq: EqType, knob: usize, k: f32) -> String {
    match knob {
        TRIM => db_text(trim_db(k)),
        CFX => match cfx(k) {
            x if x == 0.0 => "OFF".into(),
            x => {
                let (left, right) = if kind == Kind::Filter {
                    ("LP", "HP")
                } else {
                    ("L", "R")
                };
                format!(
                    "{} {:.0}%",
                    if x < 0.0 { left } else { right },
                    x.abs() * 100.0
                )
            }
        },
        _ => match eq_db(k, eq) {
            db if db <= EQ_KILL_DB => "KILL".into(),
            db => db_text(db),
        },
    }
}

/// Text under a master / headphone knob.
pub fn bus_readout(knob: usize, k: f32) -> String {
    match knob {
        HP_MIX if k <= 0.0 => "CUE".into(),
        HP_MIX if k >= 1.0 => "MASTER".into(),
        HP_MIX => format!("{:.0}% MST", k * 100.0),
        _ => db_text(bus_db(k)),
    }
}

/// Text under a STEM ISO knob (position = gain 0..1).
pub fn iso_readout(k: f32) -> String {
    if k <= 0.0 {
        "MUTE".into()
    } else {
        format!("{:.0}%", k * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_gain_knob_is_a_linear_offset_with_a_centre_detent() {
        assert_eq!(auto_adjust_db(CENTRE + 0.01), 0.0); // detent
        assert!((auto_adjust_db(1.0) - MAX_GAIN_DB).abs() < 1e-5);
        assert!((auto_adjust_db(0.0) + MAX_GAIN_DB).abs() < 1e-5);
        assert!((auto_knob(auto_adjust_db(0.8)) - 0.8).abs() < 1e-5);
        assert_eq!(auto_readout(2.0), "+2.0 dB");
    }

    #[test]
    fn trim_endpoints_and_centre() {
        assert_eq!(trim_db(0.0), f32::NEG_INFINITY);
        assert_eq!(db_to_gain(trim_db(0.0)), 0.0);
        assert_eq!(trim_db(CENTRE), 0.0);
        assert!((trim_db(1.0) - 12.0).abs() < 1e-5);
        assert!((db_to_gain(trim_db(1.0)) - 3.981).abs() < 1e-3);
        assert!((trim_db(0.25) + 30.0).abs() < 1e-4);
    }

    #[test]
    fn eq_endpoints_centre_and_kill() {
        let iso = EqType::Isolator;
        assert_eq!(eq_db(0.0, iso), EQ_KILL_DB);
        assert_eq!(eq_db(CENTRE, iso), 0.0);
        assert!((eq_db(1.0, iso) - 6.0).abs() < 1e-5);
        assert_eq!(readout(Kind::Filter, iso, LOW, 0.0), "KILL");
        assert_eq!(readout(Kind::Filter, iso, HI, CENTRE), "+0.0 dB");
        // monotonic, so dragging up never lowers a band
        for eq in [EqType::Eq, iso] {
            let mut prev = f32::NEG_INFINITY;
            for i in 0..=100 {
                let db = eq_db(i as f32 / 100.0, eq);
                assert!(db > prev);
                prev = db;
            }
        }
    }

    #[test]
    fn eq_type_eq_bottoms_out_above_the_kill() {
        let eq = EqType::Eq;
        assert!((eq_db(0.0, eq) - (EQ_KILL_DB + 0.5)).abs() < 1e-5);
        assert!(eq_db(0.0, eq) > EQ_KILL_DB); // the player's gain() is 0 only at EQ_KILL_DB
        assert_eq!(readout(Kind::Filter, eq, LOW, 0.0), "-25.5 dB");
        // centre and the top do not depend on the type
        assert_eq!(
            (eq_db(CENTRE, eq), eq_db(1.0, eq)),
            (
                eq_db(CENTRE, EqType::Isolator),
                eq_db(1.0, EqType::Isolator)
            )
        );
    }

    #[test]
    fn cfx_is_signed_and_off_at_centre() {
        assert_eq!(cfx(0.0), -1.0);
        assert_eq!(cfx(CENTRE), 0.0);
        assert_eq!(cfx(1.0), 1.0);
        assert_eq!(readout(Kind::Filter, EqType::Eq, CFX, 0.25), "LP 50%");
        assert_eq!(readout(Kind::Filter, EqType::Eq, CFX, 1.0), "HP 100%");
        assert_eq!(readout(Kind::Filter, EqType::Eq, CFX, CENTRE), "OFF");
        assert_eq!(readout(Kind::DubEcho, EqType::Eq, CFX, 0.25), "L 50%");
        assert_eq!(readout(Kind::Crush, EqType::Eq, CFX, 1.0), "R 100%");
        assert_eq!(readout(Kind::Crush, EqType::Eq, CFX, CENTRE), "OFF");
        assert_eq!(readout(Kind::Filter, EqType::Eq, TRIM, 0.0), "-inf");
    }

    #[test]
    fn bus_knobs_map_to_levels_and_text() {
        assert_eq!(db_to_gain(bus_db(0.0)), 0.0);
        assert_eq!(bus_db(CENTRE), 0.0);
        assert!((bus_db(1.0) - 6.0).abs() < 1e-5);
        assert!(db_to_gain(bus_db(1.0)) <= openfader::audio::MAX_LEVEL);
        assert_eq!(
            (
                bus_readout(MASTER, 0.0).as_str(),
                bus_readout(MASTER, CENTRE).as_str()
            ),
            ("-inf", "+0.0 dB")
        );
        assert_eq!(
            (
                bus_readout(HP_MIX, 0.0).as_str(),
                bus_readout(HP_MIX, 1.0).as_str(),
                bus_readout(HP_MIX, 0.25).as_str()
            ),
            ("CUE", "MASTER", "25% MST")
        );
        assert_eq!(BUS_START.map(snap), BUS_START); // start positions survive the detent snap
    }

    #[test]
    fn detent_snaps_near_centre_only() {
        assert_eq!(snap(0.51), CENTRE);
        assert_eq!(snap(0.49), CENTRE);
        assert_eq!(snap(0.53), 0.53);
        assert_eq!((snap(-3.0), snap(9.0), snap(f32::NAN)), (0.0, 1.0, CENTRE));
    }

    #[test]
    fn stem_isolator_text_is_the_gain_in_percent() {
        assert_eq!(
            (
                iso_readout(0.0).as_str(),
                iso_readout(1.0).as_str(),
                iso_readout(0.375).as_str()
            ),
            ("MUTE", "100%", "38%")
        );
    }
}
