use super::*;

const RATE: u32 = 44100;
/// One beat of the 120 BPM grid, in frames.
const BEAT: usize = 22050;

/// A track of 200 beats at 120 BPM from 0 ms, a drum loop controller and the callback that plays them, 512 frames at a time.
struct Rig {
    c: Controls,
    v: Voice,
    grid: Vec<u32>,
    pos: f64,
}

impl Rig {
    fn new(pos: f64) -> Rig {
        Rig {
            c: Controls::default(),
            v: Voice::new(RATE),
            grid: (0..200).map(|i| 500 * i).collect(),
            pos,
        }
    }

    /// `frames` output frames with the deck moving `kr` source frames per output frame; the left channel.
    fn run_link(&mut self, frames: usize, kr: f64, link: Link) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        while out.len() < frames {
            let n = 512.min(frames - out.len());
            self.v.begin(&self.c, link, n, RATE);
            for i in 0..n {
                out.push(
                    self.v
                        .tick(Some((&self.grid, RATE as f64)), self.pos, kr, i)[0],
                );
                self.pos += kr;
            }
            self.v.end(&self.c);
        }
        out
    }

    fn run(&mut self, frames: usize, kr: f64) -> Vec<f32> {
        self.run_link(frames, kr, Link::Held)
    }
}

/// A 4-beat loop at 120 BPM: a `len`-frame burst of 1.0 on its first beat, of 0.5 on the other three.
fn loop4(len: usize) -> Drum {
    let mut samples = vec![0.0; 2 * 4 * BEAT];
    for b in 0..4 {
        for f in b * BEAT..b * BEAT + len {
            samples[2 * f] = if b == 0 { 1.0 } else { 0.5 };
            samples[2 * f + 1] = samples[2 * f];
        }
    }
    Drum::new(
        Pcm {
            rate: RATE,
            samples,
        },
        120.0,
    )
    .unwrap()
}

/// A 4-beat loop at 120 BPM of constant `v`.
fn flat(v: f32) -> Drum {
    Drum::new(
        Pcm {
            rate: RATE,
            samples: vec![v; 2 * 4 * BEAT],
        },
        120.0,
    )
    .unwrap()
}

/// Frames where `x` rises through `level` (the first frame of each burst).
fn onsets(x: &[f32], level: f32) -> Vec<usize> {
    (1..x.len())
        .filter(|&i| x[i].abs() > level && x[i - 1].abs() <= level)
        .collect()
}

fn near(a: usize, b: usize, tol: usize) -> bool {
    a.abs_diff(b) <= tol
}

#[test]
fn a_loop_starts_on_the_next_beat_and_repeats_on_the_grid_to_the_sample() {
    let mut r = Rig::new(10_000.0); // 0.45 of the way into beat 0
    r.c.load(0, Some(loop4(8)));
    r.c.press(0);
    let x = r.run(3 * 4 * BEAT, 1.0);
    let first = BEAT - 10_000; // the deck reaches beat 1 here
    let loud = onsets(&x, 0.9);
    assert_eq!(
        loud[0], first,
        "the loop starts on the very frame the deck reaches the beat"
    );
    for (k, want) in [first + 4 * BEAT, first + 8 * BEAT].into_iter().enumerate() {
        assert!(
            near(loud[k + 1], want, 1),
            "loop {} starts at {} not {want}: {loud:?}",
            k + 1,
            loud[k + 1]
        );
    }
    assert!(
        x[..first].iter().all(|v| *v == 0.0),
        "nothing sounds before the beat"
    );
    let half = onsets(&x, 0.4);
    for (k, h) in half[..9].iter().enumerate() {
        assert!(
            near(*h, first + k * BEAT, 1),
            "beat {k} of the loop is on the deck's beat: {half:?}"
        );
    }
    assert_eq!(r.c.state(0), State::Playing);
}

#[test]
fn a_loop_follows_a_deck_at_another_tempo_without_drift() {
    for rate in [1.02, 0.92] {
        let mut r = Rig::new(10_000.0);
        r.c.load(0, Some(loop4(300)));
        r.c.press(0);
        let x = r.run(4 * 4 * BEAT, rate);
        // The deck reaches beat 1 after (BEAT - 10000) / rate output frames; the loop's beat 0 is on every 4th beat from there.
        let want = |k: usize| ((BEAT - 10_000 + k * 4 * BEAT) as f64 / rate) as usize;
        let hits = onsets(&x, 0.5);
        for k in 0..3 {
            assert!(
                hits.iter().any(|&h| near(h, want(k), 700)),
                "rate {rate}: loop {k} should start near frame {} (stretcher search window), onsets {hits:?}",
                want(k)
            );
        }
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 1.6));
    }
}

#[test]
fn quantized_jumps_leave_the_loop_playing_in_time_and_others_stop_it() {
    let mut r = Rig::new(10_000.0);
    r.c.load(0, Some(loop4(8)));
    r.c.press(0);
    let mut x = r.run(BEAT * 2, 1.0);
    r.pos += 8.0 * BEAT as f64; // a 8-beat jump (quantized loop, beat jump)
    x.extend(r.run(BEAT * 6, 1.0));
    let first = BEAT - 10_000;
    let loud = onsets(&x, 0.9);
    assert_eq!(loud[0], first);
    assert!(
        near(loud[1], first + 4 * BEAT, 1),
        "the loop did not notice the jump: {loud:?}"
    );
    r.pos += BEAT as f64 / 2.0; // half a beat off the grid
    let after = r.run(BEAT, 1.0);
    assert_eq!(r.c.state(0), State::Idle, "BEAT SYNC lost: the loop stops");
    assert!(after[1000..].iter().all(|v| *v == 0.0) && !r.c.swapping());
}

#[test]
fn stopping_fades_out_within_10_ms_and_a_second_press_stops() {
    let mut r = Rig::new(0.0);
    r.c.load(0, Some(flat(0.8)));
    r.c.press(0);
    r.run(BEAT + 1000, 1.0);
    assert_eq!(r.c.state(0), State::Playing);
    r.c.press(0);
    let x = r.run(2000, 1.0);
    assert_eq!(r.c.state(0), State::Idle);
    assert!(
        x[0] > 0.7 && x[1000] == 0.0,
        "a ramp, not a cut: {} .. {}",
        x[0],
        x[1000]
    );
    assert!(x.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01), "no click");
}

#[test]
fn the_deck_stopping_or_playing_backwards_stops_every_loop() {
    let mut r = Rig::new(0.0);
    r.c.load(0, Some(loop4(1)));
    r.c.load(1, Some(loop4(1)));
    r.c.set_multi(true);
    r.c.press(0);
    r.c.press(1);
    r.run(BEAT * 2, 1.0);
    assert_eq!([r.c.state(0), r.c.state(1)], [State::Playing; 2]);
    r.run_link(600, 1.0, Link::Lost);
    assert_eq!([r.c.state(0), r.c.state(1)], [State::Idle; 2]);
    r.c.press(0);
    r.run_link(600, 1.0, Link::Lost);
    r.run(BEAT * 2, 1.0);
    assert_eq!(
        r.c.state(0),
        State::Idle,
        "a press while sync cannot be held is ignored"
    );
    r.c.press(0);
    r.run(BEAT + 10, 1.0);
    assert_eq!(r.c.state(0), State::Playing);
    let x = r.run_link(10, 1.0, Link::Stopped);
    assert!(
        x.iter().all(|v| *v == 0.0) && r.c.state(0) == State::Idle,
        "a stopped deck drops the loops at once"
    );
}

#[test]
fn single_mode_plays_one_slot_multi_several_and_a_bank_twin_never_pairs() {
    let mut r = Rig::new(0.0);
    for i in 0..SLOTS {
        r.c.load(i, Some(loop4(1)));
    }
    r.c.press(0);
    r.run(BEAT + 5, 1.0);
    r.c.press(2);
    r.run(2 * BEAT, 1.0);
    assert_eq!(
        [r.c.state(0), r.c.state(2)],
        [State::Idle, State::Playing],
        "SINGLE: the new slot replaces the playing one"
    );
    r.c.set_multi(true);
    r.c.press(3);
    r.c.press(5); // bank B's slot 2
    r.run(2 * BEAT, 1.0);
    assert_eq!(
        [2, 3, 5].map(|i| r.c.state(i)),
        [State::Playing; 3],
        "MULTI: slots play together"
    );
    r.c.press(6); // twin of slot 2 (bank A slot 3 = index 2; bank B slot 3 = index 6)
    r.run(2 * BEAT, 1.0);
    assert_eq!(
        [r.c.state(2), r.c.state(6)],
        [State::Idle, State::Playing],
        "the same slot of bank A and B cannot play together"
    );
}

#[test]
fn swapping_is_set_only_while_a_single_mode_loop_plays() {
    let mut r = Rig::new(0.0);
    r.c.load(0, Some(loop4(1)));
    r.c.press(0);
    r.run(BEAT / 2, 1.0);
    assert!(!r.c.swapping(), "waiting for the beat: the drums play on");
    r.run(BEAT, 1.0);
    assert!(r.c.swapping());
    r.c.set_multi(true);
    r.run(512, 1.0);
    assert!(!r.c.swapping(), "MULTI keeps the drums stem on");
    r.c.set_multi(false);
    r.c.stop(0);
    r.run(1000, 1.0);
    assert!(!r.c.swapping());
}

#[test]
fn gains_scale_the_loop_and_a_new_loop_replaces_a_playing_one() {
    let level = |slot: f32, master: f32| {
        let mut r = Rig::new(0.0);
        r.c.load(0, Some(flat(0.5)));
        r.c.set_gain(0, slot);
        r.c.set_master(master);
        r.c.press(0);
        r.run(BEAT + 2000, 1.0).last().copied().unwrap()
    };
    assert!(
        (level(1.0, 1.0) - 0.5).abs() < 1e-6
            && (level(0.5, 1.0) - 0.25).abs() < 1e-6
            && (level(0.5, 0.5) - 0.125).abs() < 1e-6
    );
    let mut r = Rig::new(0.0);
    r.c.load(0, Some(loop4(1)));
    r.c.press(0);
    r.run(BEAT + 5, 1.0);
    r.c.load(0, Some(loop4(1)));
    r.run(100, 1.0);
    assert_eq!(
        r.c.state(0),
        State::Idle,
        "loading into a playing slot stops it"
    );
    r.c.load(0, None);
    assert!(r.c.drum(0).is_none());
}

#[test]
fn without_a_beat_grid_nothing_can_start() {
    let mut r = Rig::new(0.0);
    r.grid.clear();
    r.c.load(0, Some(loop4(1)));
    r.c.press(0);
    let n = 512;
    r.v.begin(&r.c, Link::Held, n, RATE);
    for i in 0..n {
        assert_eq!(r.v.tick(None, i as f64, 1.0, i), [0.0; 2]);
    }
    r.v.end(&r.c);
    assert_eq!(r.c.state(0), State::Idle);
}

#[test]
fn a_loop_is_at_most_32_beats_and_a_tempo_must_make_sense() {
    let secs = |s: usize| Pcm {
        rate: 1000,
        samples: vec![0.0; 2 * 1000 * s],
    };
    assert_eq!(
        Drum::new(secs(60), 120.0).unwrap().beats,
        32,
        "a longer track loops from its start by 32 beats"
    );
    assert_eq!(Drum::new(secs(2), 120.0).unwrap().beats, 4);
    assert_eq!(
        Drum::new(secs(1), 90.0).unwrap().beats,
        2,
        "1.5 beats round to 2"
    );
    assert!(
        Drum::new(secs(2), 0.0).is_err()
            && Drum::new(secs(2), f32::NAN).is_err()
            && Drum::new(secs(2), 1000.0).is_err()
    );
    assert!(Drum::new(Pcm::default(), 120.0).is_err());
}

#[test]
fn a_long_source_keeps_only_the_32_beats_that_play() {
    let long = Pcm {
        rate: 1000,
        samples: vec![0.25; 2 * 1000 * 600],
    }; // ten minutes
    let d = Drum::new(long, 120.0).unwrap(); // 32 beats = 16 s
    assert!(
        d.pcm.samples.len() <= 2 * 16_002 && d.pcm.samples.len() >= 2 * 16_000,
        "{}",
        d.pcm.samples.len()
    );
}

#[test]
fn drum_release_fx_acts_on_the_loop_only_in_single_mode() {
    let run = |multi: bool, on: bool| {
        let mut r = Rig::new(0.0);
        r.c.load(0, Some(flat(0.5)));
        r.c.set_multi(multi);
        r.c.release.set_effect(Effect::Trans);
        r.c.release.set_beat(2); // 1/4
        r.c.release.set_level(1.0);
        r.c.release.set_on(on);
        r.v.prepare_fx(&r.c);
        r.c.press(0);
        let x = r.run(2 * BEAT, 1.0);
        x[BEAT + 1000..]
            .iter()
            .fold((f32::MAX, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    };
    let plain = run(false, false);
    assert!(
        (plain.0 - 0.5).abs() < 1e-6 && (plain.1 - 0.5).abs() < 1e-6,
        "not held: the loop is untouched"
    );
    let gated = run(false, true);
    assert!(
        gated.0 < 0.1 && gated.1 > 0.4,
        "held in SINGLE mode: the transformer gates the loop {gated:?}"
    );
    let multi = run(true, true);
    assert!(
        (multi.0 - 0.5).abs() < 1e-6,
        "MULTI: the effect is for the DRUMS stem, not the loops {multi:?}"
    );
}
