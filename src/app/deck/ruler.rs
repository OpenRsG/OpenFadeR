//! The bar ruler of the enlarged waveform (`Theme.wave-ruler`, ui/stage/stage.slint): the numbers that let Slint draw a row of
//! bar labels over the waveform without Rust drawing anything. Per deck and frame only five numbers go to `DeckScroll`: the
//! number of the first labelled bar, where its line is, the distance between labels, and the number of the last bar. Slint repeats
//! a `Text` at that distance. Bars are numbered from 1 at the grid's first downbeat (`bar_pos == 1`, as the grid editor and the
//! phrases count them); the bar length is taken at the first visible bar line, so a tempo change inside one view drifts the
//! labels by the difference (the lines of the waveform itself stay exact).

use openfader::analysis::Beat;

/// Room a label needs to the right of its bar line, logical px (labels at least this far apart; a label whose line is
/// just off the left edge still shows).
pub(super) const LABEL_PX: f64 = 44.0;
/// Largest label step in bars (1, 2, 4 … 64).
const MAX_STEP: i32 = 64;

/// What `DeckScroll` carries for the ruler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Ruler {
    /// Number of the first labelled bar (a multiple of `step`, plus 1).
    pub first: i32,
    /// x of its line from the left edge of the view, logical px (may be a little negative).
    pub x: f32,
    /// Distance between two labels, logical px (`step` bars).
    pub pitch: f32,
    /// Bars between two labels.
    pub step: i32,
    /// The grid's last bar number: no label past it.
    pub last: i32,
}

/// The ruler of a view `width` logical px wide that shows `span_ms` of track around `pos_ms`; `None` without a grid (or when the
/// tempo cannot be read at the first visible bar).
pub(super) fn ruler(beats: &[Beat], pos_ms: f64, span_ms: f64, width: f64) -> Option<Ruler> {
    if beats.len() < 2 || span_ms <= 0.0 || width <= 0.0 {
        return None;
    }
    let per_ms = width / span_ms;
    let left = pos_ms - span_ms / 2.0;
    let from = left - LABEL_PX / per_ms;
    // A grid without any bar-1 beat counts every `per`th beat from its first, as `analysis::bars` does.
    let per = super::grid::per_bar(beats);
    let marked = beats.iter().any(|b| b.bar_pos == 1);
    let is_bar = |i: usize| {
        if marked {
            beats[i].bar_pos == 1
        } else {
            i % usize::from(per) == 0
        }
    };
    let last = (0..beats.len()).filter(|&i| is_bar(i)).count() as i32;
    let start = beats.partition_point(|b| f64::from(b.time_ms) < from);
    let before = (0..start).filter(|&i| is_bar(i)).count() as i32;
    let line = (start..beats.len()).find(|&i| is_bar(i))?;
    let next = (line + 1..beats.len()).find(|&i| is_bar(i));
    // The bar's length: to the next bar line, else a bar's beats of the last interval.
    let bar_ms = match next {
        Some(n) => f64::from(beats[n].time_ms) - f64::from(beats[line].time_ms),
        None => {
            let i = line.min(beats.len() - 2);
            f64::from(per) * (f64::from(beats[i + 1].time_ms) - f64::from(beats[i].time_ms))
        }
    };
    let bar_px = bar_ms * per_ms;
    if !(bar_px.is_finite() && bar_px > 0.0) {
        return None;
    }
    let mut step = 1;
    while step < MAX_STEP && f64::from(step) * bar_px < LABEL_PX + 8.0 {
        step *= 2;
    }
    // The first labelled bar at or after this line: bar numbers 1, 1 + step, 1 + 2 × step …
    let n0 = before + 1;
    let first = n0 + (step - (n0 - 1).rem_euclid(step)) % step;
    let x = (f64::from(beats[line].time_ms) - left + f64::from(first - n0) * bar_ms) * per_ms;
    Some(Ruler {
        first,
        x: x as f32,
        pitch: (f64::from(step) * bar_px) as f32,
        step,
        last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 BPM from 1000 ms, four beats a bar, `n` beats.
    fn grid(n: u32) -> Vec<Beat> {
        (0..n)
            .map(|i| Beat {
                time_ms: 1000 + 500 * i,
                bar_pos: (i % 4 + 1) as u16,
                bpm_x100: 12000,
            })
            .collect()
    }

    #[test]
    fn the_first_bar_line_in_view_gets_its_number_offset_and_pitch() {
        // 8 s over 800 px = 0.1 px/ms, the view starts at 6000 ms; bar lines every 2000 ms from 1000 ms (bar 1).
        let r = ruler(&grid(400), 10_000.0, 8000.0, 800.0).unwrap();
        // 44 px = 440 ms of margin: the line at 7000 ms (bar 4) is the first at or after 5560 ms.
        assert_eq!((r.first, r.step, r.last), (4, 1, 100));
        assert!((r.x - 100.0).abs() < 0.01, "{}", r.x);
        assert!((r.pitch - 200.0).abs() < 0.01, "{}", r.pitch);
    }

    #[test]
    fn zooming_out_labels_every_second_bar_then_every_fourth() {
        let g = grid(400);
        // 0.025 px/ms: a bar is 50 px, narrower than a label with its gap → every 2nd bar (1, 3, 5 …).
        let r = ruler(&g, 20_000.0, 16_000.0, 400.0).unwrap();
        assert_eq!(r.step, 2);
        assert_eq!((r.first - 1) % 2, 0);
        assert!((r.pitch - 100.0).abs() < 0.01);
        // 0.0125 px/ms: a bar is 25 px → every 4th bar; a label line stays on a bar line.
        let r = ruler(&g, 20_000.0, 32_000.0, 400.0).unwrap();
        assert_eq!(r.step, 4);
        assert_eq!((r.first - 1) % 4, 0);
        let left = 20_000.0 - 16_000.0;
        let ms = f64::from(r.x) / 0.0125 + left;
        let bar_start = 1000.0 + 2000.0 * f64::from(r.first - 1);
        assert!((ms - bar_start).abs() < 1.0, "{ms} vs {bar_start}");
    }

    #[test]
    fn labels_follow_the_playhead_pixel_by_pixel() {
        let g = grid(400);
        let a = ruler(&g, 10_000.0, 8000.0, 800.0).unwrap();
        let b = ruler(&g, 10_100.0, 8000.0, 800.0).unwrap();
        // 100 ms later the same line sits 10 px further left.
        assert_eq!(a.first, b.first);
        assert!((a.x - b.x - 10.0).abs() < 0.01);
    }

    #[test]
    fn a_zoom_that_shows_less_than_a_bar_still_numbers_it_and_a_missing_grid_hides_the_ruler() {
        let r = ruler(&grid(400), 10_500.0, 2000.0, 800.0).unwrap();
        assert_eq!(r.step, 1);
        assert!((r.pitch - 800.0).abs() < 0.01);
        assert_eq!(ruler(&[], 1000.0, 8000.0, 800.0), None);
        assert_eq!(ruler(&grid(400), 1000.0, 0.0, 800.0), None);
    }

    #[test]
    fn bars_before_the_first_downbeat_are_not_numbered_and_the_first_bar_is_one() {
        // The grid starts on beat 3 of a bar: its first bar line is at index 2.
        let mut g = grid(40);
        for (i, b) in g.iter_mut().enumerate() {
            b.bar_pos = ((i + 2) % 4 + 1) as u16;
        }
        // the playhead at the very start: the first labelled line is bar 1 at beats[2] = 2000 ms
        let r = ruler(&g, 1000.0, 8000.0, 800.0).unwrap();
        assert_eq!(r.first, 1);
        let left = 1000.0 - 4000.0;
        assert!((f64::from(r.x) - (2000.0 - left) * 0.1).abs() < 0.01);
    }

    #[test]
    fn a_three_four_grid_has_a_bar_line_every_three_beats() {
        let g: Vec<Beat> = grid(300)
            .into_iter()
            .enumerate()
            .map(|(i, b)| Beat { bar_pos: (i % 3 + 1) as u16, ..b })
            .collect();
        // 0.1 px/ms: a bar is 1500 ms = 150 px; the first line at or after 5560 ms is bar 5 at 7000 ms
        let r = ruler(&g, 10_000.0, 8000.0, 800.0).unwrap();
        assert_eq!((r.first, r.last), (5, 100));
        assert!((r.pitch - 150.0).abs() < 0.01, "{}", r.pitch);
    }
}
