//! The 8 pad views of a mode. Pad colour: `Cue.color` (RGB), else the deck's hot-cue green via
//! `render::cue_color`. A newly set hot cue takes pad `n`'s colour from `COLORFUL`, the default
//! hot-cue colours (documented there).

use super::{KEY_SHIFTS, LOOPS, PAD_FX, PadMode, hot, keyboard_shift, page_jump, page_shift};
use crate::app::{
    PadView,
    deck::{fmt_ms, is_loop_len, pad_label},
    render::cue_color,
};
use openfader::analysis::{Analysis, Cue};
use openfader::audio::{self, fx::BEAT_TEXT};
use slint::Color;

/// Semitone label: "+3", "-2", "0".
fn semitones(n: i32) -> String {
    if n == 0 { "0".into() } else { format!("{n:+}") }
}

/// The 8 pads of `mode`. Lit pads (`set`) are the set cues, the current keyboard shift (`key_st`), the
/// active loop's length (`loop_beats`); beat-jump pads are always filled. `page`: see [`super::page_range`].
/// Modes whose lit state lives on the deck come back dark (see `Deck::pad_views`).
pub(super) fn mode_views(
    mode: PadMode,
    a: &Analysis,
    key_st: i32,
    loop_beats: Option<f64>,
    page: i32,
) -> Vec<PadView> {
    let pad = |label: String, [r, g, b]: [u8; 3], set| PadView {
        label: label.into(),
        color: Color::from_rgb_u8(r, g, b),
        set,
        ..Default::default()
    };
    let numbered = |c| (1..=8).map(|n| pad(n.to_string(), c, false)).collect();
    match mode {
        PadMode::HotCue | PadMode::Plugin => views(a),
        PadMode::BeatJump => (0..8)
            .map(|i| page_jump(i, page))
            .map(|n| {
                let c = if n < 0 {
                    [255, 110, 110]
                } else {
                    [110, 220, 140]
                };
                pad(format!("{n:+}"), c, true)
            })
            .collect(),
        PadMode::Keyboard => (0..8)
            .map(|i| page_shift(keyboard_shift(i), page))
            .map(|n| pad(semitones(n), [255, 140, 20], n == key_st))
            .collect(),
        PadMode::BeatLoop => LOOPS
            .iter()
            .map(|&(n, l)| {
                let lit = loop_beats.is_some_and(|len| is_loop_len(len, n));
                pad(l.into(), [255, 153, 0], lit)
            })
            .collect(),
        PadMode::PadFx => PAD_FX
            .iter()
            .skip(8 * page.clamp(0, 1) as usize)
            .take(8)
            .map(|&(name, _, beat)| {
                pad(
                    format!("{}\n{}", crate::app::i18n::tr_dyn(name), BEAT_TEXT[beat]),
                    [230, 60, 160],
                    false,
                )
            })
            .collect(),
        PadMode::KeyShift => KEY_SHIFTS
            .iter()
            .map(|&n| page_shift(n, page))
            .map(|n| pad(semitones(n), [170, 90, 255], n == key_st))
            .collect(),
        PadMode::SampleScratch => numbered([0, 200, 200]),
        PadMode::Slicer => numbered([240, 200, 40]),
        PadMode::SlicerLoop => numbered([255, 120, 20]),
        PadMode::Sampler => numbered([150, 90, 255]),
        PadMode::SeqCall => numbered([230, 80, 120]),
        PadMode::Stems => (0..8)
            .map(|i| match i {
                // Theme.stem-vocal / stem-inst / stem-bass / stem-drums: the same colours as the V I B D buttons and the ISO knobs.
                0 => pad(tr!("VOCAL").into(), [0x02, 0xda, 0x0c], false),
                1 => pad(tr!("INST").into(), [0xda, 0x1b, 0x02], false),
                2 => pad(tr!("BASS").into(), [0xff, 0xa6, 0x00], false),
                3 => pad(tr!("DRUMS").into(), [0x46, 0x55, 0xff], false),
                i => pad(
                    tr!(
                        "DBL\n{}",
                        crate::app::i18n::tr_dyn(audio::STEM_NAMES[i - audio::STEM_COUNT])
                    ),
                    [90, 60, 120],
                    false,
                ),
            })
            .collect(),
    }
}

/// Always 8 pads A to H from hot cues 1 to 8; memory cues (0) and hot cues above 8 are ignored.
pub(in crate::app::deck) fn views(a: &Analysis) -> Vec<PadView> {
    cue_pads(|i| hot(a, i))
}

/// Pads A to H, pad `i` showing the cue `cue(i)` if there is one.
pub(in crate::app::deck) fn cue_pads<'a>(cue: impl Fn(usize) -> Option<&'a Cue>) -> Vec<PadView> {
    (0..8)
        .map(|i| {
            let label = pad_label(i).to_string().into();
            match cue(i) {
                Some(c) => {
                    let [r, g, b] = cue_color(c);
                    PadView {
                        label,
                        color: Color::from_rgb_u8(r, g, b),
                        set: true,
                        time: fmt_ms(c.time_ms).into(),
                    }
                }
                None => PadView {
                    label,
                    color: Color::from_rgb_u8(28, 28, 28),
                    set: false,
                    ..Default::default()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_views(
        mode: PadMode,
        a: &Analysis,
        key_st: i32,
        loop_beats: Option<f64>,
    ) -> Vec<PadView> {
        super::mode_views(mode, a, key_st, loop_beats, 0)
    }

    fn labels(v: &[PadView]) -> Vec<String> {
        v.iter().map(|p| p.label.to_string()).collect()
    }

    fn lit(v: &[PadView]) -> Option<usize> {
        v.iter().position(|p| p.set)
    }

    fn cue(hot: u16, ms: u32, color: Option<[u8; 3]>) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color,
            comment: None,
        }
    }

    #[test]
    fn pages_scale_beat_jumps_and_slide_the_semitone_windows() {
        let a = Analysis::default();
        let l = |m, k, p| labels(&super::mode_views(m, &a, k, None, p));
        assert_eq!(
            l(PadMode::BeatJump, 0, 2)[..4],
            ["-16", "+16", "-32", "+32"]
        );
        assert_eq!(
            l(PadMode::Keyboard, 5, 1),
            ["-2", "-1", "0", "+1", "+2", "+3", "+4", "+5"]
        );
        assert_eq!(
            lit(&super::mode_views(PadMode::Keyboard, &a, 5, None, 1)),
            Some(7)
        );
        assert_eq!(l(PadMode::KeyShift, 0, -1)[..2], ["+3", "+4"]);
        let pad_fx = |p| labels(&super::mode_views(PadMode::PadFx, &a, 0, None, p));
        assert_eq!((pad_fx(0).len(), pad_fx(1).len()), (8, 8));
        assert_ne!(
            pad_fx(0),
            pad_fx(1),
            "the SHIFT page has its own effects I to P"
        );
    }

    #[test]
    fn pads_map_with_gap_and_skip_memory_cues() {
        let a = Analysis {
            cues: vec![
                cue(0, 5, None),
                cue(1, 100, Some([1, 2, 3])),
                cue(2, 200, None),
                cue(4, 400, None),
            ],
            ..Default::default()
        };
        let v = views(&a);
        assert_eq!(v.len(), 8);
        let set: Vec<bool> = v.iter().map(|p| p.set).collect();
        assert_eq!(set, [true, true, false, true, false, false, false, false]);
        assert_eq!(v[2].label.as_str(), "C");
        assert_eq!(v[0].color, Color::from_rgb_u8(1, 2, 3));
        assert_eq!(v[1].color, Color::from_rgb_u8(40, 220, 80)); // default hot-cue colour
    }

    #[test]
    fn beat_jump_pads_are_signed_pairs_up_to_8() {
        let v = mode_views(PadMode::BeatJump, &Analysis::default(), 0, None);
        assert_eq!(labels(&v), ["-1", "+1", "-2", "+2", "-4", "+4", "-8", "+8"]);
        assert!(v.iter().all(|p| p.set));
        let red = Color::from_rgb_u8(255, 110, 110);
        let green = Color::from_rgb_u8(110, 220, 140);
        for (i, p) in v.iter().enumerate() {
            assert_eq!(p.color, if i % 2 == 0 { red } else { green });
        }
        // pads.slint's 4x2 grid shows pad i at column i/2, row i%2: -n over +n
        for col in 0..4 {
            assert_eq!(page_jump(2 * col, 0), -page_jump(2 * col + 1, 0));
        }
    }

    #[test]
    fn keyboard_pads_light_the_current_shift_only_inside_the_pad_range() {
        let a = Analysis::default();
        let v = mode_views(PadMode::Keyboard, &a, 2, None);
        assert_eq!(labels(&v), ["-3", "-2", "-1", "0", "+1", "+2", "+3", "+4"]);
        assert_eq!(lit(&v), Some(5));
        // KEY +/- can go further than the pads show
        assert_eq!(lit(&mode_views(PadMode::Keyboard, &a, 9, None)), None);
    }

    #[test]
    fn beat_loop_pads_light_the_active_length() {
        let a = Analysis::default();
        let v = mode_views(PadMode::BeatLoop, &a, 0, Some(4.01));
        assert_eq!(labels(&v), ["1/8", "1/4", "1/2", "1", "2", "4", "8", "16"]);
        assert_eq!(lit(&v), Some(5));
        assert_eq!(lit(&mode_views(PadMode::BeatLoop, &a, 0, None)), None);
        assert_eq!(lit(&mode_views(PadMode::BeatLoop, &a, 0, Some(3.0))), None);
    }

    #[test]
    fn hot_cue_mode_is_the_cue_pads() {
        let a = Analysis {
            cues: vec![cue(2, 200, None)],
            ..Default::default()
        };
        let v = mode_views(PadMode::HotCue, &a, 0, None);
        assert_eq!(labels(&v)[..2], ["A", "B"]);
        assert_eq!(lit(&v), Some(1));
    }

    #[test]
    fn key_shift_pads_light_the_current_shift() {
        let a = Analysis::default();
        let v = mode_views(PadMode::KeyShift, &a, 0, None);
        assert_eq!(labels(&v), ["+4", "+5", "+6", "+7", "0", "+1", "+2", "+3"]);
        assert_eq!(lit(&v), Some(4));
        assert_eq!(lit(&mode_views(PadMode::KeyShift, &a, 6, None)), Some(2));
        assert_eq!(lit(&mode_views(PadMode::KeyShift, &a, -2, None)), None);
    }

    #[test]
    fn modes_lit_by_the_deck_come_back_dark() {
        let a = Analysis::default();
        let v = mode_views(PadMode::PadFx, &a, 0, None);
        assert_eq!(labels(&v)[..3], ["ECHO\n1/2", "DLY\n3/4", "REV\n1"]);
        for m in [
            PadMode::SampleScratch,
            PadMode::Slicer,
            PadMode::SlicerLoop,
            PadMode::Sampler,
            PadMode::SeqCall,
            PadMode::PadFx,
        ] {
            assert!(mode_views(m, &a, 0, None).iter().all(|p| !p.set), "{m:?}");
        }
        assert_eq!(
            labels(&mode_views(PadMode::Slicer, &a, 0, None)),
            ["1", "2", "3", "4", "5", "6", "7", "8"]
        );
    }
}
