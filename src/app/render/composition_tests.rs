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
