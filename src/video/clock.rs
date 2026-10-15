//! Playhead → frame. The deck's audible position (ms into the track, already tempo-scaled by the player: at +6 % it
//! advances 1.06 ms per ms of wall time) is the video time, so tempo needs no term here; pictures go by faster.

/// Video file time (ms) at deck position `pos_ms`; `offset_ms` delays the picture against the audio (positive = later).
pub fn video_time(pos_ms: f64, offset_ms: f64) -> f64 {
    pos_ms - offset_ms
}

/// Index of the frame on screen at `t_ms` for a file of `fps` and `frames` frames; `None` before the start and past the
/// end (black).
pub fn frame_index(t_ms: f64, fps: f64, frames: i64) -> Option<i64> {
    if !t_ms.is_finite() || t_ms < 0.0 {
        return None;
    }
    // The epsilon keeps 40 ms at 25 fps on frame 1 instead of 0.999…
    let i = (t_ms * fps / 1000.0 + 1e-6).floor() as i64;
    (i < frames).then_some(i)
}

/// Start time (ms) of frame `i`.
pub fn frame_ms(i: i64, fps: f64) -> f64 {
    i as f64 * 1000.0 / fps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_picks_the_frame_it_sits_in() {
        assert_eq!(frame_index(0.0, 25.0, 100), Some(0));
        assert_eq!(frame_index(39.9, 25.0, 100), Some(0));
        assert_eq!(frame_index(40.0, 25.0, 100), Some(1));
        assert_eq!(frame_index(2000.0, 25.0, 100), Some(50));
        // 29.97 fps: frame 30 starts at 1001 ms
        assert_eq!(frame_index(1001.0, 30000.0 / 1001.0, 1000), Some(30));
        assert_eq!(frame_index(1000.0, 30000.0 / 1001.0, 1000), Some(29));
    }

    #[test]
    fn before_the_start_and_past_the_end_is_black() {
        assert_eq!(frame_index(-1.0, 25.0, 100), None);
        assert_eq!(frame_index(3999.0, 25.0, 100), Some(99));
        assert_eq!(frame_index(4000.0, 25.0, 100), None);
        assert_eq!(frame_index(f64::NAN, 25.0, 100), None);
        // a delayed picture starts later
        assert_eq!(frame_index(video_time(500.0, 1000.0), 25.0, 100), None);
        assert_eq!(frame_index(video_time(1040.0, 1000.0), 25.0, 100), Some(1));
    }

    #[test]
    fn tempo_speeds_the_frames_up_with_the_playhead() {
        // One second of wall time at +6 % moves the playhead 1060 ms: 26 or 27 frames go by, not 25.
        let at =
            |wall_ms: f64, rate: f64| frame_index(5000.0 + wall_ms * rate, 25.0, 1000).unwrap();
        assert_eq!(at(1000.0, 1.0) - at(0.0, 1.0), 25);
        assert_eq!(at(1000.0, 1.06) - at(0.0, 1.06), 26);
        assert_eq!(at(1000.0, 0.9) - at(0.0, 0.9), 22);
        // paused: the picture stays; reversed: frames count down
        assert_eq!(at(1000.0, 0.0), at(0.0, 0.0));
        assert!(at(1000.0, -1.0) < at(0.0, -1.0));
        assert_eq!(frame_ms(50, 25.0), 2000.0);
    }
}
