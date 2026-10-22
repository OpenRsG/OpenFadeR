//! Whole-track overview: bottom-anchored waveform, bar lines, phrase band, cue ticks, playhead.
//!
//! Source chain, in the order of the PREFS waveform mode ([`super::sources`]; default 3-band):
//! `three_band_preview` (1200 entries, 3-band style), `color_preview` (colour, 1200
//! entries, [`color_preview`]), `preview` (400 entries, mono blue). The entries span
//! the whole track.
//! With the STEM waveform on (PREFS) and a part muted, the 3-band wave of the stems heard ([`wave_3band`]) replaces these.

use super::{
    Axis, Canvas, Col, Layers, Src, Wave, cue_letter, hot_cue_rgb, mono_layers,
    phrase::phrase_band, playhead, slip_ghost, sources, three_band,
};
use crate::app::prefs::{
    self,
    view::{CueMarkers, OverviewSize},
};
use openfader::analysis::Analysis;

/// Colour preview → two layers (bytes 0..2 "whiteness" are ignored):
/// bytes 3,4,5 are R,G,B energies, byte 5 is also the front height; the back layer is
/// `max(b3,b4,b5)` tall at 75% brightness, the front layer `b5` tall at full brightness, each
/// colour normalised by the back height. Heights are scaled so the tallest column fills the view
/// (our choice).
fn color_preview(src: &[[u8; 6]]) -> Vec<Layers> {
    let max = src
        .iter()
        .map(|c| c[3].max(c[4]).max(c[5]))
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let layer = |c: &[u8; 6], level: f32, h: u8| {
        let back = c[3].max(c[4]).max(c[5]).max(1) as f32;
        Col {
            h: h as f32 / max,
            rgb: [c[3], c[4], c[5]].map(|v| v as f32 * level / back),
        }
    };
    src.iter()
        .map(|c| {
            [
                layer(c, 191.0, c[3].max(c[4]).max(c[5])),
                layer(c, 255.0, c[5]),
                Default::default(),
            ]
        })
        .collect()
}

/// The overview wave in the current waveform mode ([`prefs::with`]).
pub fn wave(a: &Analysis, duration_ms: u32) -> Option<Wave> {
    let layers = sources(prefs::with(|p| p.waveform_colors))
        .into_iter()
        .map(|s| match s {
            Src::ThreeBand => three_band(&a.three_band_preview),
            Src::Color => color_preview(&a.color_preview),
            Src::Mono => a.preview.iter().map(|&v| mono_layers(v)).collect(),
        })
        .find(|l: &Vec<Layers>| !l.is_empty())?;
    (duration_ms > 0).then(|| Wave {
        per_ms: layers.len() as f64 / duration_ms as f64,
        layers,
    })
}

/// The overview wave of 3-band `[low, mid, high]` entries (the STEM waveform, see `deck::stemwave`), whatever the waveform mode.
pub fn wave_3band(preview: &[[u8; 3]], duration_ms: u32) -> Option<Wave> {
    (duration_ms > 0 && !preview.is_empty()).then(|| Wave {
        per_ms: preview.len() as f64 / duration_ms as f64,
        layers: three_band(preview),
    })
}

fn axis(w: usize, duration_ms: u32) -> Axis {
    Axis {
        start_ms: 0.0,
        px_per_ms: w as f64 / duration_ms as f64,
    }
}

/// Static layer at `w`×`h` physical pixels; [`with_playhead`] adds the moving part. The bottom 14/50 of the
/// rows are the phrase strip (Theme.h-phrase of h-overview + h-phrase), the wave fills the rest.
pub fn render(wave: Option<&Wave>, a: &Analysis, duration_ms: u32, w: usize, h: usize) -> Canvas {
    let strip = if h >= 24 { h * 14 / 50 } else { 0 };
    let hw = h - strip;
    let view = prefs::with(|p| p.view.clone());
    let mut c = Canvas::new(w, hw);
    if duration_ms == 0 {
        return c.extended(strip);
    }
    let axis = axis(w, duration_ms);
    // Whole-track bars can be closer than a bold marker's width; keep them thin and behind the audio.
    for b in a.beats.iter().filter(|b| b.bar_pos == 1) {
        c.vline(axis.col(b.time_ms as f64), 0..hw, [48, 48, 48]);
    }
    if let Some(wave) = wave {
        // VIEW > Full / Half waveform: Full is symmetric around the middle row, Half grows from the bottom.
        wave.draw(&mut c, &axis, view.overview == OverviewSize::Full);
    }
    if view.vocal_full {
        for r in &a.vocal {
            c.shade(
                axis.col(r.start_ms as f64),
                axis.col(r.end_ms as f64),
                hw.saturating_sub(3)..hw,
                [2, 218, 12],
                0.8,
            ); // Theme.stem-vocal
        }
    }
    let large = view.cue_markers == CueMarkers::Large;
    for cue in &a.cues {
        let (x, rgb) = (
            axis.col(cue.time_ms as f64),
            hot_cue_rgb(view.hot_cue_colors, cue),
        );
        if cue.hot_cue == 0 {
            c.memory_cue(
                x,
                2 + usize::from(large),
                rgb,
                (hw >= 20).then_some(0),
                large,
            );
            continue;
        }
        c.vline(x, 0..hw, rgb);
        if hw >= 20 {
            c.cue_box(x, 0, cue_letter(cue.hot_cue as u8), rgb, large);
        }
    }
    let mut c = c.extended(strip);
    if view.phrase_full {
        phrase_band(&mut c, &axis, a, hw..h, None);
    }
    c
}

/// Centre x of the 2-px playhead; the overview only needs a redraw when this changes.
pub fn playhead_x(w: usize, pos_ms: f64, duration_ms: u32) -> i64 {
    axis(w, duration_ms).x(pos_ms).round().clamp(1.0, w as f64) as i64
}

/// `slip_ms`: the slip shadow playhead, drawn as a ghost line under the playhead.
pub fn with_playhead(
    base: &Canvas,
    pos_ms: f64,
    cue_ms: u32,
    slip_ms: Option<f64>,
    duration_ms: u32,
) -> slint::Image {
    let mut c = base.clone();
    if duration_ms > 0 {
        let (x, size) = (
            axis(c.w, duration_ms).col(cue_ms as f64),
            (c.h / 3).clamp(3, 8),
        );
        super::cue_marker(&mut c, x, size);
        if let Some(s) = slip_ms {
            let x = playhead_x(c.w, s, duration_ms);
            slip_ghost(&mut c, x);
        }
        let x = playhead_x(c.w, pos_ms, duration_ms);
        playhead(&mut c, x);
    }
    c.image()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::view::BeatGrid;
    use openfader::analysis::Beat;

    #[test]
    fn bold_preference_preserves_overview_dynamics_at_narrow_and_full_widths() {
        let a = Analysis {
            beats: (0..1200)
                .map(|i| Beat {
                    time_ms: i * 500,
                    bar_pos: (i % 4 + 1) as u16,
                    bpm_x100: 12000,
                })
                .collect(),
            ..Default::default()
        };
        let wave = Wave {
            layers: (0..1200)
                .map(|i| super::super::single([80, 90, 100], if i < 600 { 0.25 } else { 0.85 }))
                .collect(),
            per_ms: 1200.0 / 600_000.0,
        };
        let old = prefs::with(|p| p.view.clone());
        for overview in [OverviewSize::Full, OverviewSize::Half] {
            for w in [96, 1200] {
                prefs::set_for_test(|p| {
                    p.view.overview = overview;
                    p.view.beat_grid = BeatGrid::Normal;
                });
                let normal = render(Some(&wave), &a, 600_000, w, 50);
                prefs::set_for_test(|p| p.view.beat_grid = BeatGrid::Bold);
                let bold = render(Some(&wave), &a, 600_000, w, 50);
                assert_eq!(normal.buf.as_bytes(), bold.buf.as_bytes(), "width {w}");
                let pixel = |x: usize| {
                    let p = bold.buf.as_slice()[9 * w + x];
                    [p.r, p.g, p.b]
                };
                assert_eq!(
                    pixel(w / 4),
                    [48; 3],
                    "quiet section keeps a subdued bar line"
                );
                assert_eq!(
                    pixel(w * 3 / 4),
                    [80, 90, 100],
                    "loud section stays visible"
                );
            }
        }
        prefs::set_for_test(|p| p.view = old);
    }

    #[test]
    fn falls_back_three_band_then_colour_then_mono() {
        let mut a = Analysis {
            preview: vec![31; 400],
            ..Default::default()
        };
        assert_eq!(wave(&a, 1000).unwrap().layers.len(), 400);
        a.color_preview = vec![[0, 0, 0, 0, 0, 60], [0, 0, 0, 120, 60, 0]];
        let w = wave(&a, 1000).unwrap();
        assert_eq!(w.layers.len(), 2);
        // blue-only column: both layers 0.5 tall, back dimmer; red/green column: no front layer
        assert_eq!(
            w.layers[0][0],
            Col {
                h: 0.5,
                rgb: [0.0, 0.0, 191.0]
            }
        );
        assert_eq!(
            w.layers[0][1],
            Col {
                h: 0.5,
                rgb: [0.0, 0.0, 255.0]
            }
        );
        assert_eq!(
            w.layers[1][0],
            Col {
                h: 1.0,
                rgb: [191.0, 95.5, 0.0]
            }
        );
        assert_eq!(w.layers[1][1].h, 0.0);
        a.three_band_preview = vec![[1, 2, 3]; 1200];
        assert_eq!(wave(&a, 1000).unwrap().layers.len(), 1200);
    }
}
