//! How fast the running device job writes and how long it still takes: the status line "Copying audio 12/40 - 18.3 MB/s - about 2
//! min left", the reason a slow stick is slow, and the speeds the next preview's time estimate learns from
//! ([`openfader::device::plan::Speeds`]).
//!
//! The timer feeds [`Pace::sample`] the job's [`Control`](openfader::plugin::device::Control) counters every 250 ms. The speed is
//! the bytes of the last [`WINDOW`]; the time left is the bytes left at that speed, or, when the plugin counts no bytes, the tracks
//! left at the pace of this phase so far.

use openfader::device::plan::Speeds;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The speed is the average of this much of the past.
const WINDOW: Duration = Duration::from_secs(5);
/// A speed needs at least this much past; the cause of a slow stick is not named before.
const SETTLE: Duration = Duration::from_millis(1500);
const SLOW_SPAN: Duration = Duration::from_secs(3);
/// Below this (bytes per second) small files are what slows a stick down: analysis and library files.
const SLOW_SMALL: f64 = 2e6;
/// Below this, the audio copy is slow.
const SLOW_AUDIO: f64 = 5e6;

/// What a phase of a job writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Audio,
    /// Analysis files and the library's own files.
    Small,
    Other,
}

/// The stage a plugin's phase text names ("Copying tracks", "Writing analysis files", "Writing the library").
pub fn stage(phase: &str) -> Stage {
    let p = phase.to_lowercase();
    if p.contains("back") || p.contains("restor") {
        Stage::Other
    } else if p.contains("analysis") || p.contains("library") || p.contains("collection") {
        Stage::Small
    } else if p.contains("cop") || p.contains("audio") || p.contains("track") {
        Stage::Audio
    } else {
        Stage::Other
    }
}

/// The job's counters at one moment.
pub struct Reading<'a> {
    pub phase: &'a str,
    pub done: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Default)]
pub struct Pace {
    samples: VecDeque<(Instant, u64)>,
    /// The phase, when it began and how many steps were done then.
    phase: Option<(String, Instant, usize)>,
    last: Option<(Instant, u64)>,
    /// Audio bytes copied and the seconds it took.
    audio: (u64, f64),
    /// Seconds spent writing small files.
    small: f64,
    /// Small files the job was expected to write (the preview's count); 0 = not known.
    files: usize,
}

impl Pace {
    pub fn new(files: usize) -> Self {
        Self {
            files,
            ..Self::default()
        }
    }

    pub fn sample(&mut self, now: Instant, r: &Reading) {
        if self.phase.as_ref().is_none_or(|(p, ..)| p != r.phase) {
            self.phase = Some((r.phase.to_owned(), now, r.done));
            self.samples.clear();
            self.last = None;
        }
        if self.samples.back().is_some_and(|s| r.bytes_done < s.1) {
            self.samples.clear();
        }
        self.samples.push_back((now, r.bytes_done));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) >= WINDOW {
            self.samples.pop_front();
        }
        if let Some((t, b)) = self.last.filter(|l| r.bytes_done >= l.1) {
            let dt = now.duration_since(t).as_secs_f64();
            match stage(r.phase) {
                Stage::Audio if r.bytes_total > 0 => {
                    self.audio.0 += r.bytes_done - b;
                    self.audio.1 += dt;
                }
                Stage::Small => self.small += dt,
                _ => {}
            }
        }
        self.last = Some((now, r.bytes_done));
    }

    /// Bytes per second over the last [`WINDOW`]; `None` while there is too little past or no byte moved yet.
    pub fn rate(&self) -> Option<f64> {
        let (first, last) = (self.samples.front()?, self.samples.back()?);
        let span = last.0.duration_since(first.0);
        (span >= SETTLE && last.1 > first.1).then(|| (last.1 - first.1) as f64 / span.as_secs_f64())
    }

    /// Seconds the job still takes.
    pub fn left(&self, now: Instant, r: &Reading) -> Option<f64> {
        if r.bytes_total > 0 {
            let rate = self.rate().filter(|v| *v > 0.0)?;
            return Some(r.bytes_total.saturating_sub(r.bytes_done) as f64 / rate);
        }
        let (_, since, done0) = self.phase.as_ref()?;
        let (spent, steps) = (
            now.duration_since(*since).as_secs_f64(),
            r.done.checked_sub(*done0)?,
        );
        (steps > 0 && spent >= 2.0 && r.total > r.done)
            .then(|| (r.total - r.done) as f64 * spent / steps as f64)
    }

    /// The status line and, when the stick is slow, what makes it slow.
    pub fn status(&self, now: Instant, r: &Reading) -> (String, Option<String>) {
        let mut line = if r.total == 0 {
            r.phase.to_owned()
        } else {
            format!("{} {}/{}", r.phase, r.done, r.total)
        };
        let rate = self.rate();
        if let (Some(v), true) = (rate, r.bytes_total > 0) {
            line += &format!(" - {}", speed(v));
        }
        if let Some(s) = self.left(now, r) {
            line += &format!(" - {}", tr!("{} left", approx(s)));
        }
        let span = self
            .samples
            .front()
            .zip(self.samples.back())
            .map_or(Duration::ZERO, |(a, b)| b.0.duration_since(a.0));
        let why = rate
            .filter(|_| span >= SLOW_SPAN)
            .and_then(|v| slow(stage(r.phase), v));
        (line, why)
    }

    /// What this job taught about the device: the audio speed and the time per small file, where it ran long enough to tell.
    pub fn speeds(&self) -> Speeds {
        Speeds {
            copy_bps: (self.audio.0 >= 4_000_000 && self.audio.1 >= 1.0)
                .then(|| self.audio.0 as f64 / self.audio.1),
            small_s: (self.files >= 10 && self.small >= 1.0)
                .then(|| self.small / self.files as f64),
            analyse_s: None,
        }
    }
}

/// "18.3 MB/s".
pub fn speed(bytes_per_s: f64) -> String {
    format!("{:.1} MB/s", bytes_per_s / 1e6)
}

/// Why a stick writing `rate` bytes per second in `stage` is slow; `None` when it is not.
pub fn slow(stage: Stage, rate: f64) -> Option<String> {
    match stage {
        Stage::Small if rate < SLOW_SMALL => Some(tr!(
            "This stick writes small files slowly (analysis files): {}. A faster stick helps",
            speed(rate)
        )),
        Stage::Audio if rate < SLOW_AUDIO => Some(tr!("This stick writes at {}", speed(rate))),
        _ => None,
    }
}

/// "about 40 s", "about 3 min", "about 1 h 5 min".
pub fn approx(secs: f64) -> String {
    let s = secs.max(0.0).ceil() as u64;
    match s {
        0..10 => tr!("a few seconds").to_owned(),
        10..60 => tr!("about {} s", s.div_ceil(5) * 5),
        60..3600 => tr!("about {} min", s.div_ceil(60)),
        _ => {
            let m = s.div_ceil(60);
            tr!("about {} h {} min", m / 60, m % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, s: f64) -> Instant {
        base + Duration::from_secs_f64(s)
    }

    fn read(phase: &str, done: usize, total: usize, b: u64, bt: u64) -> Reading<'_> {
        Reading {
            phase,
            done,
            total,
            bytes_done: b,
            bytes_total: bt,
        }
    }

    #[test]
    fn the_rate_is_the_bytes_of_the_last_five_seconds() {
        let (base, mut p) = (Instant::now(), Pace::default());
        for s in 0..=20 {
            p.sample(
                at(base, s as f64),
                &read("Copying tracks", 1, 9, s * 10_000_000, 900_000_000),
            );
        }
        assert_eq!(p.rate(), Some(10e6));
        for s in 21..=30 {
            let b = 200_000_000 + (s - 20) * 1_000_000;
            p.sample(
                at(base, s as f64),
                &read("Copying tracks", 1, 9, b, 900_000_000),
            );
        }
        let r = p.rate().unwrap();
        assert!((1e6..2e6).contains(&r), "the fast past is forgotten: {r}");
    }

    #[test]
    fn there_is_no_rate_before_a_second_and_a_half_or_without_moving_bytes() {
        let (base, mut p) = (Instant::now(), Pace::default());
        p.sample(base, &read("Copying tracks", 0, 9, 0, 100));
        p.sample(at(base, 1.0), &read("Copying tracks", 0, 9, 50, 100));
        assert_eq!(p.rate(), None);
        p.sample(at(base, 2.0), &read("Copying tracks", 0, 9, 100, 100));
        assert_eq!(p.rate(), Some(50.0));
        let mut q = Pace::default();
        for s in 0..5 {
            q.sample(at(base, s as f64), &read("Writing library", 0, 1, 7, 7));
        }
        assert_eq!(q.rate(), None, "a phase that counts no bytes has no speed");
    }

    #[test]
    fn the_time_left_is_the_bytes_left_at_the_rate_else_the_tracks_left_at_the_phase_pace() {
        let (base, mut p) = (Instant::now(), Pace::default());
        for s in 0..=4 {
            p.sample(
                at(base, s as f64),
                &read("Copying tracks", 3, 40, s * 10_000_000, 100_000_000),
            );
        }
        let r = read("Copying tracks", 3, 40, 40_000_000, 100_000_000);
        assert_eq!(p.left(at(base, 4.0), &r), Some(6.0));
        // No bytes counted: 10 of 40 tracks in 20 s → 30 left take 60 s.
        let mut q = Pace::default();
        q.sample(base, &read("Writing tracks", 0, 40, 0, 0));
        let r = read("Writing tracks", 10, 40, 0, 0);
        q.sample(at(base, 20.0), &r);
        assert_eq!(q.left(at(base, 20.0), &r), Some(60.0));
        let r = read("Writing tracks", 0, 40, 0, 0);
        assert_eq!(q.left(at(base, 1.0), &r), None, "nothing done yet");
    }

    #[test]
    fn the_status_line_shows_progress_speed_and_time_left() {
        let (base, mut p) = (Instant::now(), Pace::default());
        let mut line = String::new();
        for s in 0..=4 {
            let r = read(
                "Copying audio",
                12,
                40,
                s * 18_300_000,
                18_300_000 * 4 + 18_300_000 * 120,
            );
            p.sample(at(base, s as f64), &r);
            line = p.status(at(base, s as f64), &r).0;
        }
        assert_eq!(line, "Copying audio 12/40 - 18.3 MB/s - about 2 min left");
        let r = read("Backing up the library", 0, 0, 0, 0);
        assert_eq!(Pace::default().status(base, &r).0, "Backing up the library");
    }

    #[test]
    fn a_slow_stick_is_told_what_is_slow_only_after_three_seconds() {
        assert_eq!(stage("Copying tracks"), Stage::Audio);
        assert_eq!(stage("Writing analysis files"), Stage::Small);
        assert_eq!(stage("Writing the library"), Stage::Small);
        assert_eq!(stage("Backing up the library"), Stage::Other);
        assert_eq!(stage("Exporting"), Stage::Other);
        assert_eq!(
            slow(Stage::Small, 0.4e6).unwrap(),
            "This stick writes small files slowly (analysis files): 0.4 MB/s. A faster stick helps"
        );
        assert_eq!(
            slow(Stage::Audio, 3.1e6).unwrap(),
            "This stick writes at 3.1 MB/s"
        );
        assert_eq!(
            (
                slow(Stage::Small, 2.5e6),
                slow(Stage::Audio, 5.0e6),
                slow(Stage::Other, 1.0)
            ),
            (None, None, None)
        );
        let (base, mut p) = (Instant::now(), Pace::default());
        let r = |b| read("Writing analysis files", 1, 9, b, 10_000_000);
        for s in 0..=2 {
            p.sample(at(base, s as f64), &r(s * 400_000));
        }
        assert_eq!(p.status(at(base, 2.0), &r(800_000)).1, None);
        p.sample(at(base, 3.0), &r(1_200_000));
        assert!(p
            .status(at(base, 3.0), &r(1_200_000))
            .1
            .unwrap()
            .contains("0.4 MB/s"));
    }

    #[test]
    fn a_job_teaches_the_audio_speed_and_the_time_per_small_file() {
        let (base, mut p) = (Instant::now(), Pace::new(100));
        for s in 0..=10 {
            p.sample(
                at(base, s as f64),
                &read("Copying tracks", 1, 9, s * 5_000_000, 50_000_000),
            );
        }
        for s in 11..=20 {
            p.sample(
                at(base, s as f64),
                &read("Writing analysis files", 1, 9, 50_000_000, 50_000_000),
            );
        }
        let k = p.speeds();
        assert_eq!(k.copy_bps, Some(5e6));
        assert!(
            (k.small_s.unwrap() - 0.09).abs() < 1e-9,
            "9 s over 100 files: {:?}",
            k.small_s
        );
        assert_eq!(
            Pace::new(0).speeds(),
            Speeds::default(),
            "too short to tell"
        );
    }

    #[test]
    fn times_are_rounded_up_to_what_a_person_says() {
        assert_eq!(approx(3.0), "a few seconds");
        assert_eq!(approx(41.0), "about 45 s");
        assert_eq!(approx(61.0), "about 2 min");
        assert_eq!(approx(7500.0), "about 2 h 5 min");
    }
}
