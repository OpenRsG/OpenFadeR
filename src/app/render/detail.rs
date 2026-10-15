//! The enlarged waveform's still picture ([`render_strip`]): a stretch of track around the playhead at the deck's zoom (2 to
//! 32 s per view, [`DEFAULT_SPAN_MS`] by default) and 150 entries/s, which `deck::frame` slides under the playhead. A phrase
//! band with the phrase names runs along the top.
//!
//! Source order follows the PREFS waveform mode ([`super::sources`]; default 3-band): `three_band_detail`
//! (drawn like the 3-band overview), `color_detail` (R/G/B as 3-bit fields scaled `c*255/7` and the 5-bit height as `h*255/31` of the column, as
//! [`analysis::color_detail_rgbh`] does), `detail` (mono blue); when the track has
//! none of them, peaks computed from the decoded audio. Plus beat grid, cues and the cue point; the playhead, slip ghost and
//! beat counter are drawn over the picture by `ui/deck-wave.slint` ([`beat_count_text`]).
//! With the STEM waveform on (PREFS) and a part muted, the 3-band wave of the stems heard ([`wave_3band`]) replaces these.

use super::{
    Axis, Canvas, Layers, Src, Wave, cue_letter, hot_cue_rgb, mono_layers, mono_rgbh,
    phrase::phrase_band, single, sources, three_band,
};
use crate::app::prefs::{self, view::BeatCount};
use openfader::analysis::{self, Analysis, Beat, Cue, DETAIL_ENTRIES_PER_SEC};
use openfader::audio::Pcm;

pub const DEFAULT_SPAN_MS: u32 = 8000;
const LOOP_RGB: super::Rgb = [255, 153, 0];
const PER_MS: f64 = DETAIL_ENTRIES_PER_SEC as f64 / 1000.0;

/// The counter right of the playhead (VIEW > Beat Count Display): `Position` = bars.beats from the first
/// beat to the playhead; `NextBars` / `NextBeats` = distance to the next memory cue (None without one ahead).
pub fn beat_count_text(
    mode: BeatCount,
    beats: &[Beat],
    cues: &[Cue],
    pos_ms: f64,
) -> Option<String> {
    let between = |from: f64, to: f64| {
        beats
            .iter()
            .filter(|b| b.time_ms as f64 > from && b.time_ms as f64 <= to)
            .count()
    };
    let next = || {
        cues.iter()
            .filter(|q| q.hot_cue == 0 && q.time_ms as f64 > pos_ms)
            .map(|q| q.time_ms)
            .min()
    };
    match mode {
        BeatCount::Position => {
            let n = between(f64::NEG_INFINITY, pos_ms);
            Some(format!("{}.{}BARS", n / 4, n % 4))
        }
        BeatCount::NextBars => next()
            .map(|t| between(pos_ms, t as f64 - 1.0))
            .map(|n| format!("{}.{}BARS", n / 4, n % 4)),
        BeatCount::NextBeats => next().map(|t| format!("{}BEATS", between(pos_ms, t as f64 - 1.0))),
    }
}

/// The detail wave in the current waveform mode ([`prefs::with`]); `pcm` (when the decode is done)
/// is the last resort when the analysis has no waveform.
pub fn wave(a: &Analysis, pcm: Option<&Pcm>) -> Option<Wave> {
    let layers = sources(prefs::with(|p| p.waveform_colors))
        .into_iter()
        .map(|s| match s {
            Src::ThreeBand => three_band(&a.three_band_detail),
            Src::Color => a
                .color_detail
                .iter()
                .map(|&v| {
                    let (rgb, h) = analysis::color_detail_rgbh(v);
                    single(rgb, h as f32 / 31.0)
                })
                .collect(),
            Src::Mono => a.detail.iter().map(|&v| mono_layers(v)).collect(),
        })
        .find(|l: &Vec<Layers>| !l.is_empty())
        .or_else(|| pcm.map(peaks))?;
    (!layers.is_empty()).then_some(Wave {
        layers,
        per_ms: PER_MS,
    })
}

/// The detail wave of 3-band `[low, mid, high]` entries at 150/s (the STEM waveform, see `deck::stemwave`), whatever the waveform mode.
pub fn wave_3band(detail: &[[u8; 3]]) -> Option<Wave> {
    (!detail.is_empty()).then(|| Wave {
        layers: three_band(detail),
        per_ms: PER_MS,
    })
}

/// Peak (both channels) per 1/150 s of decoded audio, fixed whiteness.
fn peaks(pcm: &Pcm) -> Vec<super::Layers> {
    let rate = pcm.rate as f64;
    let frames = pcm.samples.len() / 2;
    let rgb = mono_rgbh(4 << 5).0;
    let from = |i: usize| (i as f64 * rate / DETAIL_ENTRIES_PER_SEC as f64) as usize;
    (0..(frames as f64 * DETAIL_ENTRIES_PER_SEC as f64 / rate).ceil() as usize)
        .map(|i| {
            let s = &pcm.samples[2 * from(i).min(frames)..2 * from(i + 1).min(frames)];
            single(rgb, s.iter().fold(0f32, |m, v| m.max(v.abs())).min(1.0))
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
    for b in &a.beats {
        let rgb = if b.bar_pos == 1 {
            [230, 40, 40] // Theme.wave-bar
        } else if grid_edit {
            [61, 139, 255] // Theme.accent-hi: the grid being edited
        } else {
            [128, 128, 128] // Theme.wave-grid (white at 50%)
        };
        c.vline(axis.col(b.time_ms as f64), 0..h, rgb);
    }
    if let Some(wave) = wave {
        wave.draw(&mut c, &axis, true);
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
    for cue in &a.cues {
        let (x, rgb) = (
            axis.col(cue.time_ms as f64),
            hot_cue_rgb(view.hot_cue_colors, cue),
        );
        c.vline(x, (0..h).step_by(2), rgb);
        if cue.hot_cue > 0 && h >= 40 {
            c.cue_box(x, box_y, cue_letter(cue.hot_cue as u8), rgb);
        }
    }
    super::cue_marker(&mut c, axis.col(cue_ms as f64), (h / 12).clamp(5, 12));
    c.image()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_cover_the_audio_at_150_per_second() {
        // 1 s at 44.1 kHz with a single full-scale spike at 0.5 s => entry 75
        let mut samples = vec![0.0; 2 * 44100];
        samples[2 * 22050] = -1.0;
        let p = peaks(&Pcm {
            rate: 44100,
            samples,
        });
        assert_eq!(p.len(), 150);
        let hs: Vec<_> = p.iter().map(|l| l[0].h).collect();
        assert_eq!(hs.iter().position(|&h| h == 1.0), Some(75));
        assert_eq!(hs.iter().filter(|&&h| h > 0.0).count(), 1);
        // non-integer frames per entry (11025 Hz => 73.5) still ends at the right entry
        assert_eq!(
            peaks(&Pcm {
                rate: 11025,
                samples: vec![0.5; 2 * 11025]
            })
            .len(),
            150
        );
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
            wave(&a, None).unwrap().layers[0]
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
        assert!(wave(&b, None).unwrap().layers[0][1].h > 0.0);
        let pcm = Pcm {
            rate: 150,
            samples: vec![0.5; 300],
        };
        assert_eq!(
            wave(&Analysis::default(), Some(&pcm)).unwrap().layers[0][0].h,
            0.5
        );
        assert!(wave(&Analysis::default(), None).is_none());
    }
}
