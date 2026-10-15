use super::*;

pub(super) const RATE: u32 = 8000;

type Buf = Vec<([f32; 2], [f32; 2])>;

/// `n` frames: L = (i + 1) / 1000, R = -L.
fn ramp(n: usize) -> Pcm {
    Pcm {
        rate: RATE,
        samples: (0..n)
            .flat_map(|i| [(i + 1) as f32 / 1000.0, -((i + 1) as f32) / 1000.0])
            .collect(),
    }
}

pub(super) fn dc(n: usize) -> Pcm {
    Pcm {
        rate: RATE,
        samples: vec![0.5; 2 * n],
    }
}

/// Renders `n` frames in buffers of `chunk`, starting from silence; returns the master path's left channel.
pub(super) fn render(s: &mut Sampler, c: &Controls, n: usize, chunk: usize) -> Vec<f32> {
    let mut out = vec![];
    while out.len() < n {
        let mut b: Buf = vec![([0.0; 2], [0.0; 2]); chunk.min(n - out.len())];
        s.add(c, &mut b);
        out.extend(b.iter().map(|f| f.0[0]));
    }
    out
}

fn setup() -> (Sampler, Controls) {
    (Sampler::new(RATE), Controls::default())
}

#[test]
fn trigger_plays_exactly_the_slice_once() {
    let (mut s, c) = setup();
    c.load(3, ramp(100), 0.0);
    assert!(
        render(&mut s, &c, 10, 10).iter().all(|&x| x == 0.0),
        "silent until pressed"
    );
    c.press(3);
    c.release(3);
    let out = render(&mut s, &c, 250, 64);
    let want: Vec<f32> = (0..100).map(|i| (i + 1) as f32 / 1000.0).collect();
    assert_eq!(out[..100], want[..]);
    assert!(out[100..].iter().all(|&x| x == 0.0));
    assert!(!c.playing(3));
    // pressed again it restarts from the first frame, also while still sounding
    c.press(3);
    render(&mut s, &c, 40, 40);
    c.press(3);
    assert_eq!(render(&mut s, &c, 100, 100), want);
}

#[test]
fn loop_wraps_seamlessly_across_buffers_and_a_second_press_stops_it() {
    let (mut s, c) = setup();
    c.load(0, ramp(10), 0.0);
    c.set_loop(0, true);
    c.press(0);
    let out = render(&mut s, &c, 47, 7); // odd buffer size: wraps land inside and on buffer edges
    for (i, x) in out.iter().enumerate() {
        assert_eq!(*x, ((i % 10) + 1) as f32 / 1000.0, "frame {i}");
    }
    assert!(c.playing(0));
    c.press(0);
    let out = render(&mut s, &c, 2 * voice::RELEASE as usize, 32);
    assert!(out[voice::RELEASE as usize..].iter().all(|&x| x == 0.0) && !c.playing(0));
}

#[test]
fn stop_fades_a_playing_loop_out_whatever_the_mode_and_ignores_idle_slots() {
    let (mut s, c) = setup();
    c.load(0, ramp(10), 0.0);
    c.set_loop(0, true);
    c.stop(0); // idle: nothing
    c.stop(99); // no such slot
    c.press(0);
    render(&mut s, &c, 20, 20);
    assert!(c.playing(0));
    c.stop(0);
    let out = render(&mut s, &c, 2 * voice::RELEASE as usize, 32);
    assert!(out[voice::RELEASE as usize..].iter().all(|&x| x == 0.0) && !c.playing(0));
}

#[test]
fn gate_plays_while_held_and_stops_on_release() {
    let (mut s, c) = setup();
    c.load(1, dc(4000), 0.0);
    c.set_gate(1, true);
    c.press(1);
    assert!(
        render(&mut s, &c, 800, 100).iter().all(|&x| x == 0.5),
        "held: keeps playing"
    );
    c.release(1);
    let out = render(&mut s, &c, 400, 100);
    let r = voice::RELEASE as usize;
    assert!(
        out[0] > 0.45 && out[r - 1] < 0.02,
        "fades out: {} .. {}",
        out[0],
        out[r - 1]
    );
    assert!(
        out[r..].iter().all(|&x| x == 0.0) && !c.playing(1),
        "silent after the release"
    );
    // a tap shorter than one buffer still sounds for that buffer
    c.press(1);
    c.release(1);
    assert!(render(&mut s, &c, 100, 100).iter().all(|&x| x == 0.5));
    assert!(
        render(&mut s, &c, 200, 100)[voice::RELEASE as usize..]
            .iter()
            .all(|&x| x == 0.0)
    );
}

#[test]
fn synced_slice_lasts_beats_times_sixty_over_the_master_tempo() {
    // 4 beats at 120 BPM = 2 s of source; master 150 BPM -> 4 * 60 / 150 = 1.6 s of output.
    let (mut s, c) = setup();
    let (beats, bpm, master) = (4.0, 120.0f32, 150.0f32);
    c.load(
        2,
        dc((beats * 60.0 / bpm as f64 * RATE as f64) as usize),
        bpm,
    );
    c.set_sync(2, true);
    c.set_bpm(master);
    c.press(2);
    let want = (beats * 60.0 / master as f64 * RATE as f64) as usize;
    let mut played = 0;
    while c.playing(2) || played == 0 {
        played += render(&mut s, &c, 1, 1)
            .iter()
            .filter(|&&x| x > 0.25)
            .count();
        assert!(played <= want + 1, "ran past {want}");
    }
    assert_eq!(played, want);
    // sync off: the slice's own length
    c.set_sync(2, false);
    c.press(2);
    let own = render(&mut s, &c, 20000, 500)
        .iter()
        .filter(|&&x| x > 0.25)
        .count();
    assert_eq!(own, (beats * 60.0 / bpm as f64 * RATE as f64) as usize);
}

#[test]
fn slots_sum_onto_the_master_path_and_the_cue_path_only_when_asked() {
    let (mut s, c) = setup();
    for i in [4, 5] {
        c.load(i, dc(100), 0.0);
        c.press(i);
    }
    c.set_volume(5, 0.5);
    c.set_master(0.5);
    let mut b: Buf = vec![([0.25, -0.25], [0.0; 2]); 50];
    s.add(&c, &mut b);
    // existing 0.25 + (0.5 + 0.25) * master 0.5
    assert!(
        b.iter()
            .all(|f| f.0 == [0.25 + 0.375, -0.25 + 0.375] && f.1 == [0.0; 2]),
        "{:?}",
        b[0]
    );
    c.set_cue(true);
    let mut b: Buf = vec![([0.0; 2], [0.0; 2]); 50];
    s.add(&c, &mut b);
    assert!(b.iter().all(|f| f.0 == [0.375; 2] && f.1 == [0.375; 2]));
}

#[test]
fn a_new_slice_replaces_the_playing_one() {
    let (mut s, c) = setup();
    c.load(0, dc(1000), 0.0);
    c.set_loop(0, true);
    c.press(0);
    render(&mut s, &c, 100, 100);
    c.load(0, ramp(10), 0.0);
    assert!(
        render(&mut s, &c, 20, 20).iter().all(|&x| x == 0.0),
        "stopped by the load"
    );
    c.press(0);
    assert_eq!(render(&mut s, &c, 3, 3), [0.001, 0.002, 0.003]);
    c.clear(0);
    assert!(render(&mut s, &c, 20, 20).iter().all(|&x| x == 0.0));
    c.press(0);
    assert!(render(&mut s, &c, 20, 20).iter().all(|&x| x == 0.0) && !c.playing(0));
}

#[test]
fn slice_gives_back_what_a_slot_holds_until_it_is_cleared_or_replaced() {
    let (mut s, c) = setup();
    assert!(
        c.slice(0).is_none() && c.slice(SLOTS).is_none(),
        "empty and out-of-range slots hold nothing"
    );
    c.load(2, ramp(10), 120.0);
    let got = c.slice(2).unwrap();
    assert_eq!(
        (got.rate, got.samples.len(), got.samples[..2].to_vec()),
        (RATE, 20, vec![0.001, -0.001])
    );
    assert!(c.slice(1).is_none(), "other slots are not touched");
    c.press(2); // playing it does not disturb the kept copy
    render(&mut s, &c, 20, 20);
    c.load(2, dc(4), 0.0);
    assert_eq!(c.slice(2).unwrap().samples, vec![0.5; 8]);
    c.clear(2);
    assert!(c.slice(2).is_none());
}
