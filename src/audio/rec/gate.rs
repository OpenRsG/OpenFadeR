//! The silence gate of a recording (Preferences > Advanced > Recordings): pure, frames in, steps out.
//!
//! * start: "Pressing the REC button" records at once; "Detecting audio input" waits (frames are dropped)
//!   until a frame reaches the threshold and starts there;
//! * split ("Automatically separate files when silence continues", 1 to 10 s): after that long below the
//!   threshold the file ends ([`Step::Split`]); the next audio opens a new one. The silence stays at the
//!   end of the finished file (ponytail: not trimmed);
//! * end: "Detecting [`END_SILENCE_S`] sec. of silence" ends the recording ([`Step::End`]); a recording that never
//!   heard anything, started by audio detection, keeps waiting.
//!
//! The three options, the 20 s and the -60 dB default are the usual ones; the gate's frame-level comparison is our choice.

use std::ops::Range;

/// Length of silence that ends a recording whose end trigger is silence.
const END_SILENCE_S: u64 = 20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Start when audio is detected (else when REC is pressed).
    pub start_on_audio: bool,
    /// End after [`END_SILENCE_S`] of silence (else when REC is pressed again).
    pub end_on_silence: bool,
    /// Frames below this level (dBFS, both channels) are silence.
    pub threshold_db: f32,
    /// Silence length (s) that separates files; `None` = one file.
    pub split_after_s: Option<u32>,
    /// Raise the level of each finished file so its peak is full scale ([`super::optimise`]).
    pub optimise: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            start_on_audio: true,
            end_on_silence: true,
            threshold_db: -60.0,
            split_after_s: Some(2),
            optimise: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Append these frames of the fed block to the current file (opening a new one when none is open).
    Write(Range<usize>),
    /// Close the current file.
    Split,
    /// Close the current file and stop the recording.
    End,
}

#[derive(PartialEq)]
enum State {
    Waiting,
    Recording,
    Ended,
}

pub struct Gate {
    o: Options,
    threshold: f32,
    split: Option<u64>,
    end: u64,
    state: State,
    /// Audio has been heard (or recording began at once): silence may end the recording.
    started: bool,
    silent: u64,
}

impl Gate {
    pub fn new(o: Options, rate: u32) -> Gate {
        let rate = rate as u64;
        Gate {
            threshold: 10f32.powf(o.threshold_db / 20.0),
            split: o.split_after_s.map(|s| s.clamp(1, 10) as u64 * rate),
            end: END_SILENCE_S * rate,
            state: if o.start_on_audio {
                State::Waiting
            } else {
                State::Recording
            },
            started: !o.start_on_audio,
            silent: 0,
            o,
        }
    }

    /// Whether a file must exist right now (recording began at once and nothing was fed yet).
    pub fn open_at_start(&self) -> bool {
        !self.o.start_on_audio
    }

    pub fn feed(&mut self, frames: &[[f32; 2]]) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut from = (self.state == State::Recording).then_some(0);
        for (i, f) in frames.iter().enumerate() {
            let loud = f[0].abs() >= self.threshold || f[1].abs() >= self.threshold;
            self.silent = if loud { 0 } else { self.silent + 1 };
            if loud && self.state == State::Waiting {
                self.state = State::Recording;
                self.started = true;
                from = Some(i);
            }
            if self.started && self.o.end_on_silence && self.silent >= self.end {
                steps.extend(from.take().map(|s| Step::Write(s..i + 1)));
                steps.push(Step::End);
                self.state = State::Ended;
                return steps;
            }
            if self.state == State::Recording && self.split == Some(self.silent) {
                steps.extend(from.take().map(|s| Step::Write(s..i + 1)));
                steps.push(Step::Split);
                self.state = State::Waiting;
            }
        }
        steps.extend(from.map(|s| Step::Write(s..frames.len())));
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 100; // 100 frames per second keeps the blocks small
    const LOUD: [f32; 2] = [0.5, 0.0];
    const QUIET: [f32; 2] = [0.0; 2];

    fn opts(start_on_audio: bool, end_on_silence: bool, split: Option<u32>) -> Options {
        Options {
            start_on_audio,
            end_on_silence,
            threshold_db: -60.0,
            split_after_s: split,
            optimise: false,
        }
    }

    fn block(parts: &[([f32; 2], usize)]) -> Vec<[f32; 2]> {
        parts
            .iter()
            .flat_map(|&(f, n)| std::iter::repeat_n(f, n))
            .collect()
    }

    #[test]
    fn button_start_writes_everything_and_never_splits_without_the_option() {
        let mut g = Gate::new(opts(false, false, None), RATE);
        assert!(g.open_at_start());
        let b = block(&[(QUIET, 5000), (LOUD, 10)]);
        assert_eq!(g.feed(&b), [Step::Write(0..5010)]);
    }

    #[test]
    fn audio_start_drops_the_silence_before_the_first_loud_frame() {
        let mut g = Gate::new(opts(true, true, None), RATE);
        assert!(!g.open_at_start());
        assert_eq!(
            g.feed(&block(&[(QUIET, 900)])),
            [],
            "20 s of leading silence does not end an armed recording"
        );
        assert_eq!(
            g.feed(&block(&[(QUIET, 40), (LOUD, 10), (QUIET, 5)])),
            [Step::Write(40..55)]
        );
        assert_eq!(
            g.feed(&block(&[(QUIET, 3)])),
            [Step::Write(0..3)],
            "a short pause stays inside the file"
        );
    }

    #[test]
    fn silence_splits_the_file_and_the_next_audio_opens_another() {
        let mut g = Gate::new(opts(true, false, Some(2)), RATE);
        let steps = g.feed(&block(&[(LOUD, 10), (QUIET, 250), (LOUD, 20)]));
        assert_eq!(
            steps,
            [Step::Write(0..210), Step::Split, Step::Write(260..280)]
        );
    }

    #[test]
    fn split_across_blocks_counts_the_silent_run() {
        let mut g = Gate::new(opts(true, false, Some(1)), RATE);
        assert_eq!(
            g.feed(&block(&[(LOUD, 10), (QUIET, 60)])),
            [Step::Write(0..70)]
        );
        assert_eq!(
            g.feed(&block(&[(QUIET, 60), (LOUD, 5)])),
            [Step::Write(0..40), Step::Split, Step::Write(60..65)]
        );
    }

    #[test]
    fn twenty_seconds_of_silence_end_the_recording() {
        let mut g = Gate::new(opts(true, true, Some(2)), RATE);
        let steps = g.feed(&block(&[(LOUD, 10), (QUIET, 2100)]));
        assert_eq!(steps[0], Step::Write(0..210), "split after 2 s of silence");
        assert_eq!(steps[1], Step::Split);
        assert_eq!(steps[2], Step::End, "the file already closed: just stop");
        assert_eq!(steps.len(), 3);
        assert_eq!(
            g.feed(&block(&[(LOUD, 10)])),
            [],
            "an ended gate writes nothing"
        );
    }

    #[test]
    fn end_without_split_keeps_the_silent_tail_in_the_file() {
        let mut g = Gate::new(opts(false, true, None), RATE);
        let steps = g.feed(&block(&[(LOUD, 10), (QUIET, 2100)]));
        assert_eq!(steps, [Step::Write(0..2010), Step::End]);
    }

    #[test]
    fn threshold_decides_what_is_silence() {
        let quiet_noise = [0.002, 0.0]; // -54 dB
        let mut g = Gate::new(
            Options {
                threshold_db: -60.0,
                ..opts(true, false, None)
            },
            RATE,
        );
        assert_eq!(g.feed(&[quiet_noise]), [Step::Write(0..1)]);
        let mut g = Gate::new(
            Options {
                threshold_db: -40.0,
                ..opts(true, false, None)
            },
            RATE,
        );
        assert_eq!(g.feed(&[quiet_noise]), []);
        let mut g = Gate::new(opts(true, false, None), RATE);
        assert_eq!(
            g.feed(&[[0.0, -0.5]]),
            [Step::Write(0..1)],
            "either channel counts"
        );
    }
}
