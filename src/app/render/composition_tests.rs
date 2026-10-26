use super::*;
use crate::app::prefs;
use openfader::analysis::{Analysis, Beat, Cue};

fn fixture() -> Analysis {
    Analysis {
        beats: (0..24)
            .map(|i| Beat {
                time_ms: i * 500,
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
            })
            .collect(),
        cues: vec![Cue {
            time_ms: 3000,
            hot_cue: 1,
            loop_end_ms: None,
            color: Some([0x44, 0xaa, 0x66]),
            comment: None,
        }],
        ..Default::default()
    }
}

#[test]
fn overview_composed_textures_match_cpu_pixels_including_overlap_and_edges() {
    let a = fixture();
    for (w, h) in [(96, 25), (680, 50), (1360, 100)] {
        let base = overview::render(None, &a, 12000, w, h);
        let (cached, head, ghost) = overview::composed(&base, 1800, 12000);
        let cached = cached.to_rgba8().unwrap();
        for pos in [0.0, 1800.0, 3000.0, 6000.0, 12000.0] {
            for slip in [None, Some(pos), Some(3000.0), Some(12000.0)] {
                let mut actual = cached.as_slice().to_vec();
                for (image, x) in [
                    (
                        Some(&ghost),
                        slip.map(|s| overview::playhead_x(w, s, 12000)),
                    ),
                    (Some(&head), Some(overview::playhead_x(w, pos, 12000) - 1)),
                ] {
                    if let (Some(image), Some(x)) = (image, x) {
                        let image = image.to_rgba8().unwrap();
                        let width = image.width() as usize;
                        for y in 0..h {
                            for dx in 0..width {
                                let p = image.as_slice()[y * width + dx];
                                let col = x + dx as i64;
                                if p.a == 255 && (0..w as i64).contains(&col) {
                                    actual[y * w + col as usize] = p;
                                }
                            }
                        }
                    }
                }
                let reference = overview::with_playhead(&base, pos, 1800, slip, 12000)
                    .to_rgba8()
                    .unwrap();
                assert_eq!(
                    actual,
                    reference.as_slice(),
                    "{w}x{h}, pos={pos}, slip={slip:?}"
                );
            }
        }
    }
}

#[test]
fn scrolling_cached_detail_preserves_grid_cues_and_loop_at_each_zoom() {
    let a = fixture();
    let old = prefs::with(|p| p.view.clone());
    // Exact integer pixel positions isolate compositing from subpixel sampling.
    for span in [2000.0, 4000.0, 8000.0, 16000.0, 32000.0] {
        for bold in [false, true] {
            prefs::set_for_test(|p| {
                p.view.beat_grid = if bold {
                    prefs::view::BeatGrid::Bold
                } else {
                    prefs::view::BeatGrid::Normal
                }
            });
            let w = 512;
            let scale = w as f64 / span;
            let strip = detail::render_strip(
                None,
                &a,
                0.0,
                scale,
                2000,
                Some((4000, 8000)),
                w * 3,
                60,
                false,
            )
            .to_rgba8()
            .unwrap();
            for offset in [0, 128, 512, 1024] {
                let direct = detail::render_strip(
                    None,
                    &a,
                    offset as f64 / scale,
                    scale,
                    2000,
                    Some((4000, 8000)),
                    w,
                    60,
                    false,
                )
                .to_rgba8()
                .unwrap();
                for y in 0..60 {
                    // Marks wider than their anchor may be clipped by the reference canvas edge.
                    assert_eq!(
                        &strip.as_slice()[y * w * 3 + offset + 16..y * w * 3 + offset + w - 16],
                        &direct.as_slice()[y * w + 16..y * w + w - 16]
                    );
                }
            }
        }
    }
    prefs::set_for_test(|p| p.view = old);
}

// Independent pre-culling reference: retain every beat/cue iteration and let the canvas clip it.
fn grid_cues_reference(a: &Analysis, axis: Axis, w: usize, h: usize) -> Image {
    use prefs::view::{BeatGrid, CueMarkers};
    let view = prefs::with(|p| p.view.clone());
    let mut c = Canvas::new(w, h);
    for b in &a.beats {
        let x = axis.col(b.time_ms as f64);
        match view.beat_grid {
            BeatGrid::Normal => c.vline(
                x,
                0..h,
                if b.bar_pos == 1 {
                    [230, 40, 40]
                } else {
                    [128; 3]
                },
            ),
            BeatGrid::Bold => c.bold_grid(x, b.bar_pos == 1),
        }
    }
    let large = view.cue_markers == CueMarkers::Large;
    let memory_w = if view.beat_grid == BeatGrid::Bold {
        6
    } else {
        2
    } + 2 * usize::from(large);
    let box_y = font::H * (h / 90).max(1) + 5;
    for cue in &a.cues {
        let x = axis.col(cue.time_ms as f64);
        let rgb = hot_cue_rgb(view.hot_cue_colors, cue);
        if cue.hot_cue == 0 {
            c.memory_cue(x, memory_w, rgb, (h >= 40).then_some(box_y), large);
        } else {
            c.vline(x, (0..h).step_by(2), rgb);
            if h >= 40 {
                c.cue_box(x, box_y, cue_letter(cue.hot_cue as u8), rgb, large);
            }
        }
    }
    cue_marker(&mut c, axis.col(u32::MAX as f64), (h / 12).clamp(5, 12));
    c.image()
}

#[test]
fn detail_viewport_culling_matches_all_markers_at_fractional_edges() {
    use prefs::view::{BeatGrid, CueMarkers};
    let mut a = fixture();
    a.beats = (0..1000)
        .map(|i| Beat {
            time_ms: i * 10,
            bar_pos: (i % 4 + 1) as u16,
            bpm_x100: 12000,
        })
        .collect();
    // Deliberately unordered: cues do not promise chronological storage.
    a.cues = [1130, 970, 1010, 1000, 990, 1140, 0, 9999]
        .into_iter()
        .enumerate()
        .map(|(i, time_ms)| Cue {
            time_ms,
            hot_cue: (i % 3) as u16,
            loop_end_ms: None,
            color: None,
            comment: None,
        })
        .collect();
    let old = prefs::with(|p| p.view.clone());
    for grid in [BeatGrid::Normal, BeatGrid::Bold] {
        for large in [false, true] {
            prefs::set_for_test(|p| {
                p.view.beat_grid = grid;
                p.view.cue_markers = if large {
                    CueMarkers::Large
                } else {
                    CueMarkers::Normal
                };
                p.view.phrase_enlarged = false;
                p.view.vocal_enlarged = false;
            });
            for start_ms in [-10.5, 0.0, 980.0, 999.9, 1000.0, 1000.1, 10000.0] {
                for px_per_ms in [0.016, 0.256, 1.0, 2.0] {
                    let axis = Axis {
                        start_ms,
                        px_per_ms,
                    };
                    let expected = grid_cues_reference(&a, axis, 128, 60).to_rgba8().unwrap();
                    let actual = detail::render_strip(
                        None,
                        &a,
                        start_ms,
                        px_per_ms,
                        u32::MAX,
                        None,
                        128,
                        60,
                        false,
                    )
                    .to_rgba8()
                    .unwrap();
                    assert_eq!(
                        actual.as_bytes(),
                        expected.as_bytes(),
                        "start={start_ms}, scale={px_per_ms}, grid={grid:?}, large={large}"
                    );
                }
            }
        }
    }
    prefs::set_for_test(|p| p.view = old);
}

#[test]
#[ignore = "release timing; run with --ignored --nocapture"]
fn benchmark_detail_viewport_metadata() {
    use std::{hint::black_box, time::Instant};
    let mut a = fixture();
    a.beats = (0..100_000)
        .map(|i| Beat {
            time_ms: i * 400,
            bar_pos: (i % 4 + 1) as u16,
            bpm_x100: 15000,
        })
        .collect();
    let old = prefs::with(|p| p.view.clone());
    prefs::set_for_test(|p| {
        p.view.beat_grid = prefs::view::BeatGrid::Bold;
        p.view.phrase_enlarged = false;
        p.view.vocal_enlarged = false;
    });
    let axis = Axis {
        start_ms: 20_000_000.0,
        px_per_ms: 0.128,
    };
    let n = 100;
    for reference in [true, false] {
        let start = Instant::now();
        for _ in 0..n {
            black_box(if reference {
                grid_cues_reference(&a, axis, 1024, 120)
            } else {
                detail::render_strip(
                    None,
                    &a,
                    axis.start_ms,
                    axis.px_per_ms,
                    u32::MAX,
                    None,
                    1024,
                    120,
                    false,
                )
            });
        }
        println!(
            "detail metadata {}: {:.3} ms/strip",
            if reference {
                "whole-track reference"
            } else {
                "viewport"
            },
            start.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
    prefs::set_for_test(|p| p.view = old);
}

#[test]
#[ignore = "release timing; run with --ignored --nocapture"]
fn benchmark_waveform_composition() {
    use std::{hint::black_box, time::Instant};
    let a = fixture();
    let base = overview::render(None, &a, 12000, 680, 50);
    let cached = overview::composed(&base, 1800, 12000);
    let n = 10000;
    let start = Instant::now();
    for i in 0..n {
        black_box(overview::with_playhead(
            &base,
            i as f64,
            1800,
            Some(3000.0),
            12000,
        ));
    }
    let cpu = start.elapsed().as_secs_f64() * 1e6 / n as f64;
    let start = Instant::now();
    for i in 0..n {
        black_box((
            &cached,
            overview::playhead_x(680, i as f64, 12000),
            overview::playhead_x(680, 3000.0, 12000),
        ));
    }
    let gpu = start.elapsed().as_secs_f64() * 1e6 / n as f64;
    println!("overview CPU overlay: {cpu:.3} us/frame; cached compositor placement: {gpu:.3} us/frame (excluding Slint/GPU)");
}
