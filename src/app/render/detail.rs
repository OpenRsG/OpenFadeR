//! The enlarged waveform's still picture ([`render_strip`]): a stretch of track around the playhead at the deck's zoom (2 to
//! 32 s per view, [`DEFAULT_SPAN_MS`] by default) and 150 entries/s, which `deck::frame` slides under the playhead. A phrase
//! band with the phrase names runs along the top.
//!
//! Source order follows the PREFS waveform mode ([`super::sources`]; default 3-band): `three_band_detail`
//! (drawn like the 3-band overview), `color_detail` (R/G/B as 3-bit fields scaled `c*255/7` and the 5-bit height as `h*255/31` of the column, as
//! [`analysis::color_detail_rgbh`] does), `detail` (mono blue); when the track has
//! none of them, peaks computed from the decoded audio. Plus beat grid, cues and the cue point; the playhead, slip ghost and
//! beat counter are drawn over the picture by `ui/deck-wave.slint` ([`beat_count_value`]).
//! With the STEM waveform on (PREFS) and a part muted, the 3-band wave of the stems heard ([`wave_3band`]) replaces these.

use super::{
    color_layer, cue_letter, hot_cue_rgb, mono_layers, mono_rgbh, phrase::phrase_band, single,
    sources, three_band, Axis, Canvas, Layers, Palette, Src, Wave,
};
use crate::app::prefs::{
    self,
    view::{BeatCount, BeatGrid, CueMarkers},
};
use openfader::analysis::{self, Analysis, Beat, Cue, DETAIL_ENTRIES_PER_SEC};
use openfader::audio::{Frames, Pcm};

pub const DEFAULT_SPAN_MS: u32 = 8000;
const LOOP_RGB: super::Rgb = [255, 153, 0];
const PER_MS: f64 = DETAIL_ENTRIES_PER_SEC as f64 / 1000.0;

/// Semantic counter value; format only when it changes, rather than on every transport frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeatCountValue {
    Bars(usize),
    Beats(usize),
}

impl BeatCountValue {
    pub fn text(self) -> String {
        match self {
            Self::Bars(n) => tr!("{}.{}BARS", n / 4, n % 4),
            Self::Beats(n) => tr!("{}BEATS", n),
        }
    }
}

/// The counter right of the playhead (VIEW > Beat Count Display): `Position` = bars.beats from the first
/// beat to the playhead; `NextBars` / `NextBeats` = distance to the next memory cue (None without one ahead).
/// Beat timestamps are ordered, as for the deck's beat-grid lookup; only the two range boundaries need searching.
pub fn beat_count_value(
    mode: BeatCount,
    beats: &[Beat],
    cues: &[Cue],
    pos_ms: f64,
) -> Option<BeatCountValue> {
    let between = |from: f64, to: f64| {
        let start = beats.partition_point(|b| b.time_ms as f64 <= from);
        let end = beats.partition_point(|b| b.time_ms as f64 <= to);
        end.saturating_sub(start)
    };
    let next = || {
        cues.iter()
            .filter(|q| q.hot_cue == 0 && q.time_ms as f64 > pos_ms)
            .map(|q| q.time_ms)
            .min()
    };
    match mode {
        BeatCount::Position => Some(BeatCountValue::Bars(between(f64::NEG_INFINITY, pos_ms))),
        BeatCount::NextBars => {
            next().map(|t| BeatCountValue::Bars(between(pos_ms, t as f64 - 1.0)))
        }
        BeatCount::NextBeats => {
            next().map(|t| BeatCountValue::Beats(between(pos_ms, t as f64 - 1.0)))
        }
    }
}

/// The detail wave in the current waveform mode ([`prefs::with`]) and the colours `p` ([`super::deck_palette`] of the deck that
/// shows it); `pcm` (when the decode is done) is the last resort when the analysis has no waveform.
pub fn wave(p: &Palette, a: &Analysis, pcm: Option<&Pcm>) -> Option<Wave> {
    let layers = sources(prefs::with(|p| p.waveform_colors))
        .into_iter()
        .map(|s| match s {
            Src::ThreeBand => three_band(p, &a.three_band_detail),
            Src::Color => a
                .color_detail
                .iter()
                .map(|&v| {
                    let (rgb, h) = analysis::color_detail_rgbh(v);
                    let col = color_layer(p, rgb.map(f32::from), h as f32 / 31.0);
                    [col, Default::default(), Default::default()]
                })
                .collect(),
            Src::Mono => a.detail.iter().map(|&v| mono_layers(p, v)).collect(),
        })
        .find(|l: &Vec<Layers>| !l.is_empty())
        .or_else(|| pcm.map(|p| peaks(&p.samples, p.rate)))?;
    (!layers.is_empty()).then_some(Wave {
        layers,
        per_ms: PER_MS,
    })
}

/// The detail wave of 3-band `[low, mid, high]` entries at 150/s (the STEM waveform, see `deck::stemwave`), whatever the waveform mode.
pub fn wave_3band(p: &Palette, detail: &[[u8; 3]]) -> Option<Wave> {
    (!detail.is_empty()).then(|| Wave {
        layers: three_band(p, detail),
        per_ms: PER_MS,
    })
}

/// Audio-only fallback (`rate` Hz), prepared off the UI thread without reading UI preferences.
pub(in crate::app) fn audio_wave<S: Frames + ?Sized>(audio: &S, rate: u32) -> Option<Wave> {
    (audio.frame_count() > 0 && rate > 0).then(|| Wave {
        layers: peaks(audio, rate),
        per_ms: PER_MS,
    })
}

/// Peak (both channels) per 1/150 s of decoded audio, fixed whiteness.
fn peaks<S: Frames + ?Sized>(audio: &S, rate: u32) -> Vec<super::Layers> {
    let rate = rate as f64;
    let frames = audio.frame_count();
    let rgb = mono_rgbh(4 << 5).0;
    let from = |i: usize| (i as f64 * rate / DETAIL_ENTRIES_PER_SEC as f64) as usize;
    (0..(frames as f64 * DETAIL_ENTRIES_PER_SEC as f64 / rate).ceil() as usize)
        .map(|i| {
            let s = audio.frames_in(from(i).min(frames)..from(i + 1).min(frames));
            single(rgb, s.flatten().fold(0f32, |m, v| m.max(v.abs())).min(1.0))
        })
        .collect()
}

/// The still part of the detail: `w`×`h` physical pixels from `start_ms` at `px_per_ms`, with the beat grid, the wave, the
/// phrase band, the loop (`loop_`, ms) shaded, the cues and the cue point. What moves with the playhead (the playhead itself,
/// the slip ghost, the beat counter) is drawn over it by `ui/deck-wave.slint`, so a playing deck only slides this picture
/// (`deck::frame`). `grid_edit` (the GRID panel is open): the beat lines are blue instead of white.
#[allow(clippy::too_many_arguments)]
pub fn render_strip(
    wave: Option<&Wave>,
    a: &Analysis,
    start_ms: f64,
    px_per_ms: f64,
    cue_ms: u32,
    loop_: Option<(u32, u32)>,
    w: usize,
    h: usize,
    grid_edit: bool,
) -> slint::Image {
    let view = prefs::with(|p| p.view.clone()); // plain-data section: a copy, not a borrow held across the frame
    let mut c = Canvas::new(w, h);
    let axis = Axis {
        start_ms,
        px_per_ms,
    };
    // Include off-screen anchors whose bold outlines still touch the viewport; beats are time-ordered.
    let first = a.beats.partition_point(|b| axis.col(b.time_ms as f64) < -4);
    let last = a
        .beats
        .partition_point(|b| axis.col(b.time_ms as f64) < w as i64 + 2);
    let beats = &a.beats[first..last];
    if view.beat_grid == BeatGrid::Normal {
        for b in beats {
            let rgb = if b.bar_pos == 1 {
                [230, 40, 40] // Theme.wave-bar
            } else if grid_edit {
                [61, 139, 255] // Theme.accent-hi: the grid being edited
            } else {
                [128, 128, 128] // Theme.wave-grid (white at 50%)
            };
            c.vline(axis.col(b.time_ms as f64), 0..h, rgb);
        }
    }
    if let Some(wave) = wave {
        wave.draw(&mut c, &axis, true);
    }
    if view.beat_grid == BeatGrid::Bold {
        for b in beats {
            c.bold_grid(axis.col(b.time_ms as f64), b.bar_pos == 1);
        }
    }
    if view.vocal_enlarged {
        // Voice regions (heuristic, `analysis::detect::vocal_regions`): a band along the bottom.
        for r in &a.vocal {
            c.shade(
                axis.col(r.start_ms as f64),
                axis.col(r.end_ms as f64),
                h.saturating_sub(5)..h,
                [2, 218, 12],
                0.8,
            ); // Theme.stem-vocal
        }
    }
    // Phrase band with names across the top, glyphs grow with the view height (our layout choice). VIEW > Phrases:
    // the band only when "Phrase (Enlarged Waveform)" is on; the names when "Always show the types of phrases" is on
    // (otherwise they are left out: a drawn image has no pointer for mouse-over).
    let scale = (h / 90).max(1);
    if view.phrase_enlarged {
        phrase_band(
            &mut c,
            &axis,
            a,
            0..super::font::H * scale + 4,
            view.phrase_names_always.then_some(scale),
        );
    }
    if let Some((s, e)) = loop_ {
        let (x0, x1) = (axis.col(s as f64), axis.col(e as f64));
        c.shade(x0, x1, 0..h, LOOP_RGB, 0.3);
        c.vline(x0, 0..h, LOOP_RGB);
        c.vline(x1, 0..h, LOOP_RGB);
    }
    let box_y = super::font::H * scale + 5;
    let large = view.cue_markers == CueMarkers::Large;
    // Memory cues: wider than a beat line, and than a bold downbeat (4 px), so one on a beat still shows.
    let memory_w = if view.beat_grid == BeatGrid::Bold {
        6
    } else {
        2
    } + 2 * usize::from(large);
    for cue in &a.cues {
        let x = axis.col(cue.time_ms as f64);
        // Large flags reach 16 px right; widened memory bars also reach left of their anchor.
        if x < -16 || x >= w as i64 + 4 {
            continue;
        }
        let rgb = hot_cue_rgb(view.hot_cue_colors, cue);
        if cue.hot_cue == 0 {
            c.memory_cue(x, memory_w, rgb, (h >= 40).then_some(box_y), large);
            continue;
        }
        c.vline(x, (0..h).step_by(2), rgb);
        if h >= 40 {
            c.cue_box(x, box_y, cue_letter(cue.hot_cue as u8), rgb, large);
        }
    }
    super::cue_marker(&mut c, axis.col(cue_ms as f64), (h / 12).clamp(5, 12));
    c.image()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_counts_at_beat_and_memory_cue_boundaries() {
        let beats: Vec<_> = [0, 500, 1000, 1000, 1500, 2000, 2500]
            .into_iter()
            .map(|time_ms| Beat {
                time_ms,
                bar_pos: 1,
                bpm_x100: 12000,
            })
            .collect();
        let cues = [(2500, 0), (500, 1), (1500, 0)].map(|(time_ms, hot_cue)| Cue {
            time_ms,
            hot_cue,
            loop_end_ms: None,
            color: None,
            comment: None,
        });
        for (pos, position, bars, count) in [
            (-1.0, "0.0BARS", Some("1.0BARS"), Some("4BEATS")),
            (0.0, "0.1BARS", Some("0.3BARS"), Some("3BEATS")),
            (499.9, "0.1BARS", Some("0.3BARS"), Some("3BEATS")),
            (500.0, "0.2BARS", Some("0.2BARS"), Some("2BEATS")),
            (1000.0, "1.0BARS", Some("0.0BARS"), Some("0BEATS")),
            (1499.5, "1.0BARS", Some("0.0BARS"), Some("0BEATS")),
            (1500.0, "1.1BARS", Some("0.1BARS"), Some("1BEATS")),
            (2499.9, "1.2BARS", Some("0.0BARS"), Some("0BEATS")),
            (2500.0, "1.3BARS", None, None),
            (3000.0, "1.3BARS", None, None),
        ] {
            for (mode, expected) in [
                (BeatCount::Position, Some(position)),
                (BeatCount::NextBars, bars),
                (BeatCount::NextBeats, count),
            ] {
                assert_eq!(
                    beat_count_value(mode, &beats, &cues, pos)
                        .map(BeatCountValue::text)
                        .as_deref(),
                    expected,
                    "position {pos}"
                );
            }
        }
        for (mode, expected) in [
            (BeatCount::Position, Some("0.0BARS")),
            (BeatCount::NextBars, Some("0.0BARS")),
            (BeatCount::NextBeats, Some("0BEATS")),
        ] {
            assert_eq!(
                beat_count_value(mode, &[], &cues, 0.0)
                    .map(BeatCountValue::text)
                    .as_deref(),
                expected
            );
        }
        for mode in [BeatCount::NextBars, BeatCount::NextBeats] {
            assert_eq!(beat_count_value(mode, &beats, &[], 0.0), None);
        }
        assert_eq!(
            beat_count_value(BeatCount::Position, &beats, &cues, 1000.0),
            Some(BeatCountValue::Bars(4))
        );
        assert_eq!(
            beat_count_value(BeatCount::Position, &beats, &cues, 1499.5),
            Some(BeatCountValue::Bars(4))
        );
        assert_eq!(
            beat_count_value(BeatCount::NextBars, &beats, &cues, 1000.0),
            Some(BeatCountValue::Bars(0))
        );
        assert_eq!(
            beat_count_value(BeatCount::NextBeats, &beats, &cues, 1000.0),
            Some(BeatCountValue::Beats(0))
        );
    }

    #[test]
    fn bold_grid_stays_visible_over_audio_and_marks_downbeats_by_width() {
        let a = Analysis {
            beats: vec![
                Beat {
                    bar_pos: 1,
                    bpm_x100: 12700,
                    time_ms: 10,
                },
                Beat {
                    bar_pos: 2,
                    bpm_x100: 12700,
                    time_ms: 30,
                },
            ],
            ..Default::default()
        };
        let wave = Wave {
            layers: vec![single([80, 90, 100], 1.0); 80],
            per_ms: 1.0,
        };
        prefs::set_for_test(|p| p.view.beat_grid = BeatGrid::Bold);
        let image = render_strip(Some(&wave), &a, 0.0, 1.0, u32::MAX, None, 80, 60, false)
            .to_rgba8()
            .unwrap();
        let pixel = |x: usize| {
            let p = image.as_slice()[40 * 80 + x];
            [p.r, p.g, p.b]
        };
        let palette = super::super::palette();
        for x in 9..13 {
            assert_eq!(pixel(x), palette.cue, "downbeat at {x}");
        }
        for x in 30..32 {
            assert_eq!(pixel(x), palette.band[2], "beat at {x}");
        }
        for x in [8, 13, 29, 32] {
            assert_eq!(pixel(x), palette.bg, "outline at {x}");
        }
        assert_eq!(pixel(20), [80, 90, 100], "audio remains between grid lines");
        prefs::set_for_test(|p| p.view.beat_grid = BeatGrid::Normal);
    }

    #[test]
    fn memory_cue_on_a_bold_downbeat_stays_visible_flagged_and_widens_with_large() {
        let a = Analysis {
            beats: vec![Beat {
                bar_pos: 1,
                bpm_x100: 12700,
                time_ms: 30,
            }],
            cues: vec![Cue {
                hot_cue: 0,
                time_ms: 30,
                loop_end_ms: None,
                color: None,
                comment: None,
            }],
            ..Default::default()
        };
        let draw = |large| {
            prefs::set_for_test(|p| {
                p.view.beat_grid = BeatGrid::Bold;
                p.view.cue_markers = if large {
                    CueMarkers::Large
                } else {
                    CueMarkers::Normal
                };
            });
            render_strip(None, &a, 0.0, 1.0, u32::MAX, None, 80, 60, false)
                .to_rgba8()
                .unwrap()
        };
        let palette = super::super::palette();
        let row = |img: &slint::SharedPixelBuffer<slint::Rgba8Pixel>, y: usize| {
            (0..80)
                .filter(|&x| {
                    let p = img.as_slice()[y * 80 + x];
                    [p.r, p.g, p.b] == palette.memory
                })
                .count()
        };
        let (normal, large) = (draw(false), draw(true));
        // Below the flag: only the bar, wider than the 4-px bold downbeat and not the grid's colour.
        assert_eq!(row(&normal, 55), 6);
        assert_eq!(row(&large, 55), 8);
        // The flag's top row: the 8×9 box (16×18 large) in the cue colour, wider than the bar.
        assert_eq!(row(&normal, 12), 8);
        assert_eq!(row(&large, 12), 16);
        // ... with the letter M cut into it
        assert!(row(&normal, 14) < 8);
        assert!(row(&large, 14) < 16);
        prefs::set_for_test(|p| {
            p.view.beat_grid = BeatGrid::Normal;
            p.view.cue_markers = CueMarkers::Normal;
        });
    }

    #[test]
    fn peaks_cover_the_audio_at_150_per_second() {
        // 1 s at 44.1 kHz with a single full-scale spike at 0.5 s => entry 75
        let mut samples = vec![0.0; 2 * 44100];
        samples[2 * 22050] = -1.0;
        let p = peaks(&samples, 44100);
        assert_eq!(p.len(), 150);
        let hs: Vec<_> = p.iter().map(|l| l[0].h).collect();
        assert_eq!(hs.iter().position(|&h| h == 1.0), Some(75));
        assert_eq!(hs.iter().filter(|&&h| h > 0.0).count(), 1);
        // non-integer frames per entry (11025 Hz => 73.5) still ends at the right entry
        assert_eq!(peaks(&vec![0.5; 2 * 11025], 11025).len(), 150);
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;
    use crate::app::prefs::{WaveColors, set_for_test};

    #[test]
    fn waveform_mode_picks_the_stored_family_and_falls_back() {
        let a = Analysis {
            three_band_detail: vec![[10, 20, 30]; 4],
            color_detail: vec![(7 << 13) | (10 << 2); 4], // pure red, height 10
            detail: vec![20; 4],                          // mono, height 20, no whiteness
            ..Default::default()
        };
        let first = |mode| {
            set_for_test(|p| p.waveform_colors = mode);
            wave(&Palette::DEFAULT, &a, None).unwrap().layers[0]
        };
        assert!(
            first(WaveColors::ThreeBand)[1].h > 0.0,
            "3-band uses the mid layer"
        );
        assert_eq!(first(WaveColors::Rgb)[0].rgb, [255.0, 0.0, 0.0]);
        assert_eq!(first(WaveColors::Blue)[0].rgb, [0.0, 104.0, 216.0]);
        // no mono data: Blue falls back to the 3-band data; nothing stored: peaks of the audio
        let b = Analysis {
            three_band_detail: a.three_band_detail.clone(),
            ..Default::default()
        };
        assert!(wave(&Palette::DEFAULT, &b, None).unwrap().layers[0][1].h > 0.0);
        let pcm = Pcm {
            rate: 150,
            samples: vec![0.5; 300],
        };
        assert_eq!(
            wave(&Palette::DEFAULT, &Analysis::default(), Some(&pcm))
                .unwrap()
                .layers[0][0]
                .h,
            0.5
        );
        assert!(wave(&Palette::DEFAULT, &Analysis::default(), None).is_none());
    }
}
