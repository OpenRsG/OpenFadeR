//! Our own phrase analysis (`analysis::auto`) against the song structure of a real device library (read by its plugin) named by
//! `OPENFADER_USB`; skips otherwise. Two runs: on the device's own beat grid + 3-band waveform (the
//! segmentation alone), and on our own analysis of the decoded audio (the whole path a folder track takes).
//! A boundary agrees when one of ours starts within one bar of one of theirs.

mod common;

use openfader::analysis::{self, Beat, Mood, Phrase, PhraseKind, Phrases, auto};
use openfader::audio::decode;

/// Start time (ms) of every phrase after the first.
fn starts(p: &Phrases, beats: &[Beat]) -> Vec<f64> {
    p.list
        .iter()
        .skip(1)
        .map(|ph| ph.span_ms(beats).0)
        .collect()
}

/// Kind at time `t`.
fn kind_at(p: &Phrases, beats: &[Beat], t: f64) -> Option<PhraseKind> {
    p.list
        .iter()
        .find(|ph| (ph.span_ms(beats).0..ph.span_ms(beats).1).contains(&t))
        .map(|ph| ph.kind)
}

#[derive(Default)]
struct Score {
    ours: usize,
    truth: usize,
    /// Boundaries of ours with one of theirs within 1 bar / 1 beat.
    prec: [usize; 2],
    /// Boundaries of theirs with one of ours within 1 bar / 1 beat.
    rec: [usize; 2],
    /// Of ours within 1 bar of one of theirs: the phrase kind agrees.
    kind_ok: usize,
    /// Milliseconds where both name the same kind / compared milliseconds.
    same_ms: f64,
    all_ms: f64,
    high: usize,
    tracks: usize,
}

impl Score {
    /// Scores `ours` against `truth`; `bar` is one bar in ms.
    fn add(&mut self, truth: &Phrases, tb: &[Beat], ours: &Phrases, ob: &[Beat], bar: f64) {
        let (t, o) = (starts(truth, tb), starts(ours, ob));
        (self.tracks, self.high) = (
            self.tracks + 1,
            self.high + (ours.mood == Mood::High) as usize,
        );
        self.truth += t.len();
        self.ours += o.len();
        for (n, tol) in [bar, bar / 4.0].into_iter().enumerate() {
            self.prec[n] += o
                .iter()
                .filter(|&&x| t.iter().any(|&y| (x - y).abs() <= tol))
                .count();
            self.rec[n] += t
                .iter()
                .filter(|&&y| o.iter().any(|&x| (x - y).abs() <= tol))
                .count();
        }
        for (i, &x) in o.iter().enumerate() {
            if let Some(j) = t.iter().position(|&y| (x - y).abs() <= bar) {
                self.kind_ok += (ours.list[i + 1].kind == truth.list[j + 1].kind) as usize;
            }
        }
        let end = truth
            .list
            .last()
            .unwrap()
            .span_ms(tb)
            .1
            .min(ours.list.last().unwrap().span_ms(ob).1);
        let mut x = 0.0;
        while x < end {
            self.all_ms += 250.0;
            self.same_ms += (kind_at(truth, tb, x) == kind_at(ours, ob, x)) as u8 as f64 * 250.0;
            x += 250.0;
        }
    }

    /// Prints the scores and returns the F1 within 1 bar.
    fn report(&self, what: &str) -> f64 {
        let f1 = |n: usize| {
            let (p, r) = (
                self.prec[n] as f64 / self.ours as f64,
                self.rec[n] as f64 / self.truth as f64,
            );
            (p * 100.0, r * 100.0, 200.0 * p * r / (p + r))
        };
        let (bar, beat) = (f1(0), f1(1));
        eprintln!(
            "{what}: {} tracks, {} of ours vs {} real boundaries; within 1 bar: precision {:.1} % recall {:.1} % F1 {:.1} %; within 1 beat: precision {:.1} % recall {:.1} % F1 {:.1} %; kind of matched phrases {:.1} %; kind by time {:.1} %; mood High {}/{}",
            self.tracks,
            self.ours,
            self.truth,
            bar.0,
            bar.1,
            bar.2,
            beat.0,
            beat.1,
            beat.2,
            self.kind_ok as f64 * 100.0 / self.prec[0].max(1) as f64,
            self.same_ms * 100.0 / self.all_ms,
            self.high,
            self.tracks
        );
        bar.2 / 100.0
    }
}

/// The dumbest plausible answer: a phrase every 8 bars from the first beat, all Chorus.
fn every_8_bars(truth: &Phrases) -> Phrases {
    let end = truth.list.last().unwrap().end_beat;
    let list = (0..)
        .map(|i| 1 + 32 * i)
        .take_while(|&b| b < end)
        .map(|b| Phrase {
            beat: b,
            end_beat: (b + 32).min(end),
            kind: PhraseKind::Chorus,
            variant: 1,
            fill: None,
        });
    Phrases {
        mood: truth.mood,
        list: list.collect(),
    }
}

#[test]
fn auto_phrases_agree_with_the_devices() {
    let Some((lib, tracks)) = common::usb_library() else {
        return;
    };
    let (mut on_ref, mut on_ours, mut base) =
        (Score::default(), Score::default(), Score::default());
    for t in &tracks {
        let a = lib.analysis(t).unwrap();
        let truth = a.phrases.as_ref().unwrap();
        let bar = 240_000.0 / (a.beats[0].bpm_x100 as f64 / 100.0);
        base.add(truth, &a.beats, &every_8_bars(truth), &a.beats, bar);
        if let Some(p) = auto::phrases(&a.beats, &a.three_band_detail) {
            on_ref.add(truth, &a.beats, &p, &a.beats, bar);
        }
        let pcm = decode(t.file.as_ref().unwrap()).unwrap();
        let mine = analysis::analyze(&pcm.samples, pcm.rate);
        if let Some(p) = &mine.phrases {
            on_ours.add(truth, &a.beats, p, &mine.beats, bar);
        }
    }
    let b = base.report("baseline (every 8 bars, all Chorus)");
    let r = on_ref.report("auto on the device's grid + waveform");
    let o = on_ours.report("auto on our grid + waveform");
    assert_eq!(
        (on_ref.tracks, on_ours.tracks),
        (tracks.len(), tracks.len()),
        "every track gets phrases"
    );
    assert!(
        r > b + 0.1 && o > b + 0.1,
        "F1 {r:.3} / {o:.3} does not beat the every-8-bars baseline {b:.3}"
    );
}
