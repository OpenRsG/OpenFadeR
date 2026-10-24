use super::super::advance;
use super::super::fx::{self, Settings};
use super::voice::STEM_RAMP_S;
use super::*;

#[test]
fn gain_ramp_endpoints() {
    let n = 480; // 10 ms at 48 kHz
    assert!((gain_at(0.2, 1.0, 0, n) - (0.2 + 0.8 / n as f32)).abs() < 1e-6); // first frame: one step from the old gain
    assert_eq!(gain_at(0.2, 1.0, n - 1, n), 1.0); // last frame is the new gain: the next buffer starts from it
    assert_eq!(gain_at(1.0, 0.0, n - 1, n), 0.0);
    assert!((0..n).all(|i| gain_at(0.7, 0.7, i, n) == 0.7));
    let ramp: Vec<f32> = (0..n).map(|i| gain_at(0.0, 1.0, i, n)).collect();
    assert!(ramp.windows(2).all(|w| w[1] > w[0]));
}

#[test]
fn loop_packing_roundtrips_and_rejects_empty() {
    assert_eq!(unpack(pack(Some((0.0, 5.0)))), Some((0.0, 5.0)));
    assert_eq!(
        unpack(pack(Some((123_456.0, 789_012.0)))),
        Some((123_456.0, 789_012.0))
    );
    assert_eq!(unpack(pack(Some((5.0, 5.0)))), None);
    assert_eq!(unpack(pack(Some((9.0, 5.0)))), None);
    assert_eq!(unpack(pack(None)), None);
}

/// Installs `pcm` as the deck's track, 16-bit like [`Player::load`].
fn set_track(sh: &Shared, pcm: Pcm) {
    *sh.pcm.lock() = Arc::new(Track16::from(pcm));
}

/// The track's samples as the callback reads them (dequantised).
fn track(sh: &Shared) -> Vec<f32> {
    let t = sh.pcm.lock();
    t.iter_f32(0..t.frames()).collect()
}

/// A playing `Shared` on a 44.1 kHz mono-sine track, rendered like the device would: 512-frame buffers.
fn rig(rate: f64, key_lock: bool) -> (Shared, Voice) {
    let sh = Shared::new();
    let n = 44100 * 5;
    let samples = (0..n)
        .flat_map(|i| {
            let v = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 44100.0).sin() as f32 * 0.5;
            [v, v]
        })
        .collect();
    set_track(
        &sh,
        Pcm {
            rate: 44100,
            samples,
        },
    );
    sh.playing.store(true, Relaxed);
    sh.rate.store(rate.to_bits(), Relaxed);
    sh.key_lock.store(key_lock, Relaxed);
    (sh, Voice::new(44100))
}

fn run_buffers(sh: &Shared, v: &mut Voice, buffers: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(buffers * 512);
    for _ in 0..buffers {
        render(sh, v, 44100, 512, |_, m, _| out.push(m[0]));
    }
    out
}

#[test]
fn jog_cutter_gate_ramps_in_1ms_and_silences_without_a_click() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 2);
    sh.cut.store(true, Relaxed);
    let out = run_buffers(&sh, &mut v, 2);
    let ramp = (0.001 * 44100.0) as usize + 1; // 1 ms (+1 for rounding)
    assert!(
        out[..ramp].iter().any(|y| y.abs() > 0.01),
        "fading, not muted instantly"
    );
    assert!(
        out[ramp..].iter().all(|&y| y == 0.0),
        "fully cut after the ramp"
    );
    assert!(
        out.windows(2).all(|w| (w[1] - w[0]).abs() < 0.1),
        "no step: a hard mute of a 0.5 sine would jump up to 0.5"
    );
    sh.cut.store(false, Relaxed);
    let out = run_buffers(&sh, &mut v, 2);
    assert!(out[ramp..].iter().any(|y| y.abs() > 0.4), "open again");
}

/// Rising zero crossings per second over the second half of `x`.
fn pitch(x: &[f32]) -> f64 {
    let x = &x[x.len() / 4..];
    x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f64 / (x.len() as f64 / 44100.0)
}

#[test]
fn key_lock_keeps_pitch_while_consuming_source_at_the_tempo() {
    let (sh, mut v) = rig(1.1, true);
    let out = run_buffers(&sh, &mut v, 86);
    let hz = pitch(&out);
    assert!((hz / 440.0 - 1.0).abs() < 0.01, "{hz:.1} Hz");
    let consumed = f64::from_bits(sh.pos.load(Relaxed));
    assert!(
        (consumed - 86.0 * 512.0 * 1.1).abs() < 1e-6,
        "consumed {consumed}"
    );
}

#[test]
fn without_key_lock_pitch_follows_rate() {
    let (sh, mut v) = rig(1.1, false);
    let hz = pitch(&run_buffers(&sh, &mut v, 86));
    assert!((hz - 484.0).abs() < 4.84, "{hz:.1} Hz");
}

#[test]
fn varispeed_path_is_untouched_by_the_chain() {
    let (sh, mut v) = rig(1.0, false);
    let out = run_buffers(&sh, &mut v, 4);
    let src = track(&sh);
    assert!(out.iter().enumerate().all(|(i, &y)| y == src[2 * i]));
}

#[test]
fn level_is_the_post_gain_peak_of_the_last_buffer() {
    let sh = Shared::new();
    set_track(
        &sh,
        Pcm {
            rate: 44100,
            samples: [0.125, -0.75]
                .repeat(256)
                .into_iter()
                .chain([0.25, 0.25].repeat(256))
                .collect(),
        },
    );
    sh.playing.store(true, Relaxed);
    sh.gain.store(0.5f32.to_bits(), Relaxed);
    sh.gain_applied.store(0.5f32.to_bits(), Relaxed);
    let mut v = Voice::new(44100);
    render(&sh, &mut v, 44100, 256, |_, _, _| {});
    assert_eq!(sh.level(), 0.375); // |-0.75| * 0.5, from the right channel
    render(&sh, &mut v, 44100, 256, |_, _, _| {});
    assert_eq!(sh.level(), 0.125);
    sh.playing.store(false, Relaxed);
    render(&sh, &mut v, 44100, 256, |_, _, _| {});
    assert_eq!(sh.level(), 0.0);
}

#[test]
fn cue_path_ignores_the_fader_gain() {
    let sh = Shared::new();
    set_track(
        &sh,
        Pcm {
            rate: 44100,
            samples: [0.5, -0.5].repeat(64),
        },
    );
    sh.playing.store(true, Relaxed);
    let (mut v, mut last) = (Voice::new(44100), ([0.0; 2], [0.0; 2]));
    // fader closed (gain 0), CUE on at unity trim: heard on the cue path only
    for g in [(0.0f32, 1.0f32), (0.25, 0.0)] {
        for a in [
            (&sh.gain, g.0),
            (&sh.gain_applied, g.0),
            (&sh.cue, g.1),
            (&sh.cue_applied, g.1),
        ] {
            a.0.store(a.1.to_bits(), Relaxed);
        }
        sh.seek.store(0f64.to_bits(), Relaxed);
        render(&sh, &mut v, 44100, 32, |_, m, c| last = (m, c));
        assert_eq!(last, ([g.0 * 0.5, g.0 * -0.5], [g.1 * 0.5, g.1 * -0.5]));
    }
}

#[test]
fn eq_kill_and_filter_reach_the_output() {
    let (sh, mut v) = rig(1.0, false);
    sh.eq[1].store(dsp::EQ_KILL_DB.to_bits(), Relaxed); // 440 Hz sits in the mid band
    let out = run_buffers(&sh, &mut v, 40);
    let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
    assert!(
        rms(&out[out.len() / 2..]) < 0.05 * 0.35,
        "killed mid left {}",
        rms(&out[out.len() / 2..])
    );
}

#[test]
fn cfx_filter_type_is_the_plain_filter_and_other_types_leave_it_off() {
    let mut plain = dsp::Fx::new();
    let (sh, mut v) = rig(1.0, false);
    sh.cfx.set_knob(-0.6);
    let out = run_buffers(&sh, &mut v, 20);
    let src = track(&sh);
    for b in 0..20 {
        plain.set(44100.0, [0.0; 3], -0.6, 512);
        for i in b * 512..(b + 1) * 512 {
            assert_eq!(
                out[i],
                plain.process(src[2 * i], src[2 * i + 1]).0,
                "frame {i}"
            );
        }
    }
    let (sh, mut v) = rig(1.0, false);
    sh.cfx.set_kind(cfx::Kind::Crush);
    sh.cfx.set_knob(0.0);
    let out = run_buffers(&sh, &mut v, 20);
    assert!(
        out.iter().enumerate().all(|(i, &y)| y == src[2 * i]),
        "centre: bit-exact bypass"
    );
}

#[test]
fn cfx_tail_rings_after_the_deck_stops() {
    let (sh, mut v) = rig(1.0, false);
    sh.cfx.set_kind(cfx::Kind::DubEcho);
    sh.cfx.set_knob(1.0);
    run_buffers(&sh, &mut v, 40);
    sh.cfx.set_knob(0.0);
    sh.playing.store(false, Relaxed);
    let out = run_buffers(&sh, &mut v, 20);
    assert!(
        out.iter().any(|&y| y.abs() > 0.05),
        "the echo repeats are still heard"
    );
}

/// Printed with `--nocapture`: wall time of one 512-frame callback with everything on.
#[test]
fn cpu_per_callback() {
    let (sh, mut v) = rig(1.08, true);
    sh.eq
        .iter()
        .zip([-6.0f32, 3.0, dsp::EQ_KILL_DB])
        .for_each(|(a, db)| a.store(db.to_bits(), Relaxed));
    sh.cfx.set_knob(-0.4);
    run_buffers(&sh, &mut v, 8);
    let t = Instant::now();
    let n = 300;
    let mut sink = 0.0f32;
    for _ in 0..n {
        render(&sh, &mut v, 44100, 512, |_, m, _| sink += m[0]);
    }
    let per = t.elapsed().as_secs_f64() / n as f64;
    println!(
        "render 512 frames, key lock + EQ + filter: {:.1} us per callback ({:.2}% of the 11.6 ms budget) {sink}",
        per * 1e6,
        per / (512.0 / 44100.0) * 100.0
    );
}

/// Printed with `--nocapture`: two decks, key shift +3 st with key lock and +8 % tempo, EQ and filter on; wall time to render 1 s of audio.
#[test]
fn cpu_two_decks_key_shift_one_second() {
    let mut decks = [rig(1.08, true), rig(1.08, false)];
    for (sh, _) in &decks {
        sh.t.set_shift(3.0);
        sh.eq
            .iter()
            .zip([-6.0f32, 3.0, 2.0])
            .for_each(|(a, db)| a.store(db.to_bits(), Relaxed));
        sh.cfx.set_knob(-0.4);
    }
    let (mut best, mut sink) = (f64::MAX, 0.0f32);
    for _ in 0..5 {
        for (sh, _) in &decks {
            sh.seek.store(0f64.to_bits(), Relaxed);
        }
        let t = Instant::now();
        for _ in 0..86 {
            for (sh, v) in &mut decks {
                render(sh, v, 44100, 512, |_, m, _| sink += m[0]);
            }
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "render 1 s of audio on 2 decks with key shift: {:.2} ms ({:.2}% of real time) {sink}",
        best * 1e3,
        best * 100.0
    );
}

#[test]
fn key_shift_changes_pitch_not_tempo() {
    let (sh, mut v) = rig(1.0, false);
    sh.t.set_shift(7.0);
    let out = run_buffers(&sh, &mut v, 86);
    let hz = pitch(&out);
    assert!(
        (hz / (440.0 * 2f64.powf(7.0 / 12.0)) - 1.0).abs() < 0.01,
        "{hz:.1} Hz"
    );
    let consumed = f64::from_bits(sh.pos.load(Relaxed));
    assert!(
        (consumed / (86.0 * 512.0) - 1.0).abs() < 0.01,
        "consumed {consumed}"
    );
    sh.t.set_shift(99.0);
    assert_eq!(sh.t.shift(), 12.0);
}

#[test]
fn key_shift_with_key_lock_ignores_tempo() {
    let (sh, mut v) = rig(1.08, true);
    sh.t.set_shift(-5.0);
    let out = run_buffers(&sh, &mut v, 86);
    let hz = pitch(&out);
    assert!(
        (hz / (440.0 * 2f64.powf(-5.0 / 12.0)) - 1.0).abs() < 0.01,
        "{hz:.1} Hz"
    );
    let consumed = f64::from_bits(sh.pos.load(Relaxed));
    assert!(
        (consumed / (86.0 * 512.0 * 1.08) - 1.0).abs() < 1e-9,
        "tempo: consumed {consumed}"
    );
}

#[test]
fn key_shift_without_key_lock_rides_on_varispeed_pitch() {
    let (sh, mut v) = rig(1.1, false);
    sh.t.set_shift(-12.0);
    let hz = pitch(&run_buffers(&sh, &mut v, 86));
    assert!((hz / (440.0 * 1.1 / 2.0) - 1.0).abs() < 0.01, "{hz:.1} Hz");
    assert!((f64::from_bits(sh.pos.load(Relaxed)) / (86.0 * 512.0 * 1.1) - 1.0).abs() < 1e-9);
}

#[test]
fn zero_shift_without_key_lock_is_bit_exact_varispeed() {
    let (sh, mut v) = rig(1.1, false);
    sh.t.set_shift(5.0);
    sh.t.set_shift(0.0);
    let out = run_buffers(&sh, &mut v, 6);
    let src = sh.pcm.lock();
    let k = step(44100.0, 1.1, 44100);
    let mut pos = 0.0;
    for (i, &y) in out.iter().enumerate() {
        assert_eq!(y, src.frame_at(pos).unwrap()[0], "frame {i}");
        pos = advance(pos, k, None);
    }
}

#[test]
fn slip_returns_to_the_shadow_after_a_four_beat_loop() {
    let (sh, mut v) = rig(1.0, false);
    sh.t.set_slip(true);
    run_buffers(&sh, &mut v, 10);
    let beat = 22050; // 0.5 s at 44.1 kHz
    sh.set_loop(Some((117.0, 117.0 + 500.0))); // 1 beat loop around the playhead (5120 frames = 116 ms)
    run_buffers(&sh, &mut v, 4 * beat / 512);
    let (s, e) = (117.0 * 44.1, 617.0 * 44.1);
    let looped = f64::from_bits(sh.pos.load(Relaxed));
    assert!((s..e).contains(&looped), "still looping: {looped}");
    let shadow = sh.t.shadow().unwrap();
    assert_eq!(
        shadow,
        (10 + 4 * beat / 512) as f64 * 512.0,
        "shadow ran at normal speed through the loop"
    );
    sh.set_loop(None); // loop ends
    let d = render(&sh, &mut v, 44100, 512, |_, _, _| {});
    assert_eq!(d.start, shadow, "released at the shadow position");
    assert_eq!(f64::from_bits(sh.pos.load(Relaxed)), shadow + 512.0);
    // nothing diverged any more: the playhead keeps running with the shadow
    run_buffers(&sh, &mut v, 3);
    assert_eq!(f64::from_bits(sh.pos.load(Relaxed)), sh.t.shadow().unwrap());
}

#[test]
fn slip_loop_without_slip_stays_in_the_loop() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 10);
    sh.set_loop(Some((117.0, 617.0)));
    run_buffers(&sh, &mut v, 200);
    sh.set_loop(None);
    run_buffers(&sh, &mut v, 1);
    assert!(f64::from_bits(sh.pos.load(Relaxed)) < 617.0 * 44.1 + 512.0);
}

#[test]
fn slip_return_after_a_seek_lands_on_the_shadow() {
    let (sh, mut v) = rig(1.0, false);
    sh.t.set_slip(true);
    run_buffers(&sh, &mut v, 4);
    sh.seek_to(100_000.0); // hot cue pressed
    run_buffers(&sh, &mut v, 6);
    assert_eq!(
        f64::from_bits(sh.pos.load(Relaxed)),
        100_000.0 + 6.0 * 512.0
    );
    assert_eq!(sh.t.shadow(), Some(10.0 * 512.0));
    sh.t.slip_return(); // released
    run_buffers(&sh, &mut v, 1);
    assert_eq!(f64::from_bits(sh.pos.load(Relaxed)), 11.0 * 512.0);
    sh.t.set_slip(false);
    assert_eq!(sh.t.shadow(), None);
}

#[test]
fn a_press_in_the_buffer_of_the_previous_release_beats_the_slip_return() {
    let (sh, mut v) = rig(1.0, false);
    sh.t.set_slip(true);
    run_buffers(&sh, &mut v, 4);
    sh.seek_to(100_000.0); // hot cue pressed
    run_buffers(&sh, &mut v, 6);
    sh.t.slip_return(); // released ...
    sh.seek_to(200_000.0); // ... and pressed again before the callback
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 200_000.0 + 512.0);
    run_buffers(&sh, &mut v, 3);
    sh.t.slip_return(); // its release goes to the shadow
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 15.0 * 512.0);
}

#[test]
fn reverse_released_under_its_own_slip_returns_to_the_shadow_then_slip_goes_off() {
    let (sh, mut v) = rig(1.0, false);
    sh.t.set_slip(true); // QUANTIZE REVERSE: REVERSE engages slip itself
    run_buffers(&sh, &mut v, 20);
    sh.set_reverse(true);
    run_buffers(&sh, &mut v, 10); // plays backwards from 20 buffers in
    sh.set_reverse(false);
    sh.t.slip_off_after_return();
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 31.0 * 512.0, "where forward play would be");
    assert!(!sh.t.slip());
}

#[test]
fn a_stopped_deck_has_no_slip_divergence_to_return_from() {
    let slip_rig = || {
        let (sh, mut v) = rig(1.0, false);
        sh.t.set_slip(true);
        run_buffers(&sh, &mut v, 10);
        sh.set_loop(Some((117.0, 617.0))); // loop roll held
        run_buffers(&sh, &mut v, 20);
        (sh, v)
    };
    // CUE while playing: pause and back to the cue point (0), then the roll is let go.
    let (sh, mut v) = slip_rig();
    sh.playing.store(false, Relaxed);
    sh.t.settle();
    sh.seek_to(0.0);
    run_buffers(&sh, &mut v, 1);
    sh.set_loop(None);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 0.0);
    // PAUSE (brake) in the roll, then the roll is let go.
    let (sh, mut v) = slip_rig();
    sh.t.brake(0.05);
    run_buffers(&sh, &mut v, 8);
    assert!(!sh.playing.load(Relaxed));
    let stopped = pos(&sh);
    sh.set_loop(None);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), stopped);
}

#[test]
fn only_the_play_button_takes_the_vinyl_start_ramp() {
    let (sh, mut v) = rig(1.0, false);
    sh.vinyl_release.store(0.25f32.to_bits(), Relaxed);
    sh.playing.store(false, Relaxed);
    sh.play(false); // hot cue / CUE / fader start / synced start
    run_buffers(&sh, &mut v, 4);
    assert_eq!(pos(&sh), 4.0 * 512.0, "full speed from the first buffer");
    sh.playing.store(false, Relaxed);
    sh.seek_to(0.0);
    sh.play(true); // PLAY
    run_buffers(&sh, &mut v, 4);
    assert!(pos(&sh) < 3.0 * 512.0, "ramping up: {}", pos(&sh));
}

#[test]
fn a_hot_cue_during_the_pause_brake_plays_on() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 4);
    sh.t.brake(0.125); // PAUSE
    run_buffers(&sh, &mut v, 2);
    sh.seek_to(100_000.0); // hot cue: seek, play
    sh.play(false);
    run_buffers(&sh, &mut v, 40);
    assert!(sh.playing.load(Relaxed));
    assert_eq!(pos(&sh), 100_000.0 + 40.0 * 512.0);
}

#[test]
fn a_phase_kept_seek_catches_up_what_played_before_it_took_effect() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 4); // 2048 frames written
    // Worked out while frame 1000 was heard (the rest still in the output latency): lands 1048 frames past the target,
    // so the next buffer starts where the target sits in the beat the listener hears.
    sh.seek_from_heard(100_000.0, Some(1000.0));
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 100_000.0 + 1048.0 + 512.0);
    // A plain seek lands on its target.
    sh.seek_to(50_000.0);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 50_512.0);
    // Heard "ahead" of what was written (a loop wrapped since the press): the press time no longer applies.
    sh.seek_from_heard(100_000.0, Some(60_000.0));
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 100_512.0);
}

/// The reference of a phase-kept seek is where the FIRST press was heard: a second press made before the first is taken only sees
/// the first one's target as the position, and must not replace it.
#[test]
fn two_phase_kept_seeks_in_a_row_both_count_the_latency_once() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 4); // 2048 frames written, 1000 of them heard
    sh.seek_from_heard(100_000.0, Some(1000.0));
    sh.seek_from_heard(200_000.0, Some(100_000.0)); // 4 beats on from the first target, as position_ms() reports it
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 200_000.0 + 1048.0 + 512.0);
}

/// A phase-kept seek made while a loop plays catches up what played meanwhile even when the loop wrapped in between, and when it
/// is gone by the time the callback takes the seek (a slicer pad's release clears the roll and seeks back).
#[test]
fn a_phase_kept_seek_counts_the_way_round_a_loop_that_wrapped_or_ended() {
    let (sh, mut v) = rig(1.0, false);
    sh.set_loop(Some((100.0, 200.0))); // 4410..8820 frames
    sh.seek_to(4410.0);
    run_buffers(&sh, &mut v, 1); // written 4922: heard 1024 frames behind = 3898, i.e. 8308 before the wrap
    sh.seek_from_heard(100_000.0, Some(8308.0));
    sh.set_loop(None);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 100_000.0 + 1024.0 + 512.0);
}

/// 1 beat at 124 BPM is 483.871 ms: as whole ms (483) the loop would run 33 ms off the grid in 19 s.
#[test]
fn a_beat_loop_keeps_its_length_to_a_fraction_of_a_ms() {
    let (sh, mut v) = rig(1.0, false);
    let beat = 60_000.0 / 124.0;
    sh.set_loop(Some((1452.0, 1452.0 + beat)));
    sh.seek_to(1452.0 * 44.1);
    run_buffers(&sh, &mut v, 1636);
    let (start, len) = (1452.0 * 44.1, beat * 44.1);
    let want = start + (1636.0 * 512.0) % len;
    assert!((pos(&sh) - want).abs() < 5.0, "{} vs {want}", pos(&sh));
}

#[test]
fn reverse_plays_the_source_backwards_and_pauses_at_the_start() {
    let (sh, mut v) = rig(1.0, false);
    sh.seek.store(44100f64.to_bits(), Relaxed);
    sh.set_reverse(true);
    let out = run_buffers(&sh, &mut v, 10);
    let src = track(&sh);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &y)| y == src[2 * (44100 - i)]),
        "mirror image of the source"
    );
    let mut last = f64::from_bits(sh.pos.load(Relaxed));
    assert_eq!(last, 44100.0 - 10.0 * 512.0);
    for _ in 0..4 {
        run_buffers(&sh, &mut v, 10);
        let p = f64::from_bits(sh.pos.load(Relaxed));
        assert!(p < last);
        last = p;
    }
    sh.seek.store(1000f64.to_bits(), Relaxed);
    run_buffers(&sh, &mut v, 4);
    assert!(!sh.playing.load(Relaxed), "reached the start: paused");
    assert_eq!(f64::from_bits(sh.pos.load(Relaxed)), 0.0);
}

#[test]
fn reverse_loop_wraps_and_key_locked_reverse_keeps_pitch() {
    let (sh, mut v) = rig(1.0, true);
    sh.set_loop(Some((100.0, 200.0))); // 4410..8820 frames
    sh.seek.store(6000f64.to_bits(), Relaxed);
    sh.set_reverse(true);
    let mut seen = vec![];
    let mut out = vec![];
    for _ in 0..86 {
        render(&sh, &mut v, 44100, 512, |_, m, _| out.push(m[0]));
        seen.push(f64::from_bits(sh.pos.load(Relaxed)));
    }
    assert!(sh.playing.load(Relaxed));
    assert!(
        seen.iter().all(|p| (4410.0..=8820.0).contains(p)),
        "stays in the loop"
    );
    assert!(
        seen.windows(2).any(|w| w[1] > w[0]),
        "wrapped from the start back to the end"
    );
    let hz = pitch(&out);
    assert!((hz / 440.0 - 1.0).abs() < 0.01, "{hz:.1} Hz"); // the 4410-frame loop holds 44 periods: no seam
}

/// "Halving a loop sometimes exits it": the UI sees the heard playhead, which lags the one the audio renders, so a loop can
/// shrink to an end the real playhead has already passed. It must play on inside the new loop, not out past it.
#[test]
fn a_loop_halved_behind_the_playhead_keeps_playing_inside_it() {
    let (sh, mut v) = rig(1.0, false);
    sh.set_loop(Some((100.0, 300.0))); // 4410..13230 frames
    sh.seek.store(10_000f64.to_bits(), Relaxed); // second half
    run_buffers(&sh, &mut v, 1);
    sh.set_loop(Some((100.0, 200.0))); // halved: the playhead is past the new end
    for _ in 0..200 {
        run_buffers(&sh, &mut v, 1);
        let p = f64::from_bits(sh.pos.load(Relaxed));
        assert!((4410.0..8820.0).contains(&p), "left the loop at {p}");
    }
}

#[test]
fn brake_ramps_the_speed_to_zero_then_pauses_in_place() {
    let (sh, mut v) = rig(1.0, true); // key lock is bypassed: pitch falls with the speed
    run_buffers(&sh, &mut v, 8);
    sh.t.brake(0.5);
    let mut pos = vec![f64::from_bits(sh.pos.load(Relaxed))];
    let mut buffers = 0;
    while sh.playing.load(Relaxed) && buffers < 200 {
        run_buffers(&sh, &mut v, 1);
        pos.push(f64::from_bits(sh.pos.load(Relaxed)));
        buffers += 1;
    }
    assert!(!sh.playing.load(Relaxed));
    let secs = buffers as f64 * 512.0 / 44100.0;
    assert!(
        (0.5..0.5 + 512.0 / 44100.0 + 1e-9).contains(&secs),
        "stopped after {secs:.3} s"
    );
    let adv: Vec<f64> = pos.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        adv.windows(2).all(|w| w[1] <= w[0] + 1e-9),
        "speed never rises: {adv:?}"
    );
    assert!(adv[0] > 0.9 * 512.0 && *adv.last().unwrap() < 0.1 * 512.0);
    let rest = *pos.last().unwrap();
    assert!(
        (rest - (8.0 * 512.0 + 0.5 * 44100.0 / 2.0)).abs() < 50.0,
        "coasted a triangle: {rest}"
    );
    run_buffers(&sh, &mut v, 5);
    assert_eq!(f64::from_bits(sh.pos.load(Relaxed)), rest, "position kept");
    // the next play runs at full speed again
    sh.playing.store(true, Relaxed);
    run_buffers(&sh, &mut v, 1);
    assert!((f64::from_bits(sh.pos.load(Relaxed)) - rest - 512.0).abs() < 1e-9);
}

const SRC: f64 = 44100.0;
const BUF: usize = 512;

/// A 10 s source whose left sample rises linearly with the frame index (0 .. 0.9): the output reveals direction and position.
fn ramp_rig(playing: bool) -> (Shared, Voice) {
    let sh = Shared::new();
    let n = 441_000;
    set_track(
        &sh,
        Pcm {
            rate: 44100,
            samples: (0..n)
                .flat_map(|i| {
                    let v = 0.9 * i as f32 / n as f32;
                    [v, v]
                })
                .collect(),
        },
    );
    sh.playing.store(playing, Relaxed);
    (sh, Voice::new(44100))
}

fn pos(sh: &Shared) -> f64 {
    f64::from_bits(sh.pos.load(Relaxed))
}

/// Platter move of `total` track seconds fed evenly over `buffers` buffers, then `settle` buffers at rest; returns the left output.
fn scratch_move(sh: &Shared, v: &mut Voice, total: f64, buffers: usize, settle: usize) -> Vec<f32> {
    let mut out = vec![];
    for b in 0..buffers + settle {
        sh.scratch_move(if b < buffers {
            total / buffers as f64
        } else {
            0.0
        });
        render(sh, v, 44100, BUF, |_, m, _| out.push(m[0]));
    }
    out
}

#[test]
fn scratch_forward_half_second_in_100ms_lands_on_target_and_plays_forward() {
    let (sh, mut v) = ramp_rig(false);
    sh.seek_to(2.0 * SRC);
    sh.scratch_begin();
    let out = scratch_move(&sh, &mut v, 0.5, 9, 24); // 9 buffers = 104 ms of motion, then 280 ms of settling
    assert!(
        (pos(&sh) - 2.5 * SRC).abs() < 0.001 * SRC,
        "landed at {} (target {})",
        pos(&sh),
        2.5 * SRC
    );
    let plateau = &out[4000..4500]; // constant speed, gate open
    // The 16-bit track steps every ~15 frames of this ramp: never falling, and rising across the plateau.
    assert!(
        plateau.windows(2).all(|w| w[1] >= w[0]) && plateau[499] > plateau[0],
        "rising ramp = playing forward"
    );
    assert!(
        out.iter().any(|&s| s > 0.2),
        "it moved from 0.18 (2.0 s) towards 0.225 (2.5 s)"
    );
}

#[test]
fn scratch_backwards_is_time_reversed() {
    let (sh, mut v) = ramp_rig(false);
    sh.seek_to(5.0 * SRC);
    sh.scratch_begin();
    let out = scratch_move(&sh, &mut v, -0.5, 9, 24);
    assert!(
        (pos(&sh) - 4.5 * SRC).abs() < 0.001 * SRC,
        "landed at {}",
        pos(&sh)
    );
    let plateau = &out[4000..4500];
    assert!(
        plateau.windows(2).all(|w| w[1] <= w[0]) && plateau[499] < plateau[0],
        "falling ramp = playing backwards"
    );
}

#[test]
fn scratch_on_a_paused_deck_sounds_and_leaves_it_paused() {
    let (sh, mut v) = ramp_rig(false);
    sh.seek_to(SRC);
    run_buffers(&sh, &mut v, 2);
    assert_eq!(pos(&sh), SRC, "paused: silent and still");
    sh.scratch_begin();
    let out = scratch_move(&sh, &mut v, 0.3, 6, 10);
    assert!(
        out.iter().any(|&s| s > 0.1),
        "audible while the platter moves"
    );
    assert!(!sh.playing.load(Relaxed));
    let at = pos(&sh);
    assert!((at - 1.3 * SRC).abs() < 44.1);
    sh.scratch_end();
    let out = run_buffers(&sh, &mut v, 4);
    assert!(
        out.iter().all(|&s| s == 0.0) && pos(&sh) == at,
        "released: paused where it was"
    );
}

#[test]
fn scratch_on_a_playing_deck_resumes_at_normal_speed_direction_and_key_lock() {
    let (sh, mut v) = rig(1.0, true);
    run_buffers(&sh, &mut v, 4);
    sh.scratch_begin();
    scratch_move(&sh, &mut v, -0.2, 4, 12);
    assert!(sh.scratch.load(Relaxed) && sh.playing.load(Relaxed));
    sh.scratch_end();
    let before = pos(&sh);
    let out = run_buffers(&sh, &mut v, 86);
    assert!(
        (pos(&sh) - before - 86.0 * 512.0).abs() < 1e-6,
        "forward at rate 1 again"
    );
    let hz = pitch(&out);
    assert!(
        (hz / 440.0 - 1.0).abs() < 0.01,
        "key lock back on: {hz:.1} Hz"
    );
}

#[test]
fn scratch_ignores_loops_and_reverse_and_never_runs_off_the_track() {
    let (sh, mut v) = ramp_rig(true);
    sh.set_loop(Some((100.0, 200.0)));
    sh.set_reverse(true);
    sh.seek_to(9.0 * SRC);
    sh.scratch_begin();
    scratch_move(&sh, &mut v, 30.0, 10, 30);
    assert!(
        pos(&sh) <= 441_000.0 - 1.0 && pos(&sh) > 441_000.0 - 100.0,
        "stopped at the end: {}",
        pos(&sh)
    );
    assert!(
        sh.playing.load(Relaxed),
        "the end of the track does not stop the deck while scratching"
    );
    scratch_move(&sh, &mut v, -60.0, 10, 30);
    assert!(pos(&sh) < 100.0, "and the start is a floor: {}", pos(&sh));
}

#[test]
fn slip_shadow_keeps_running_under_a_scratch_and_the_release_returns_to_it() {
    let (sh, mut v) = ramp_rig(true);
    sh.t.set_slip(true);
    run_buffers(&sh, &mut v, 4);
    sh.scratch_begin();
    scratch_move(&sh, &mut v, -0.3, 4, 4);
    assert_eq!(
        sh.t.shadow(),
        Some(12.0 * 512.0),
        "shadow at normal speed through 8 scratch buffers"
    );
    assert!(pos(&sh) < 4.0 * 512.0, "playhead went back");
    sh.scratch_end();
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 13.0 * 512.0, "back on the shadow");
}

#[test]
fn nudge_bends_the_speed_only_while_set_and_leaves_the_tempo_alone() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh), 512.0);
    sh.nudge.store(0.1f32.to_bits(), Relaxed);
    run_buffers(&sh, &mut v, 1);
    let bent = pos(&sh) - 512.0;
    assert!((bent - 512.0 * 1.1).abs() < 1e-3, "{bent}");
    assert_eq!(
        f64::from_bits(sh.rate.load(Relaxed)),
        1.0,
        "tempo value untouched"
    );
    sh.nudge.store(0f32.to_bits(), Relaxed);
    let at = pos(&sh);
    run_buffers(&sh, &mut v, 1);
    assert_eq!(pos(&sh) - at, 512.0);
}

/// Printed with `--nocapture`: one 512-frame callback while scratching with key lock + EQ + filter armed (all bypassed or cheap).
#[test]
fn cpu_per_callback_while_scratching() {
    let (sh, mut v) = rig(1.08, true);
    sh.eq
        .iter()
        .zip([-6.0f32, 3.0, dsp::EQ_KILL_DB])
        .for_each(|(a, db)| a.store(db.to_bits(), Relaxed));
    sh.cfx.set_knob(-0.4);
    sh.seek_to(2.0 * SRC);
    sh.scratch_begin();
    let n = 300;
    let mut sink = 0.0f32;
    let t = Instant::now();
    for i in 0..n {
        sh.scratch_move(0.02 * if i % 2 == 0 { 1.0 } else { -1.0 });
        render(&sh, &mut v, 44100, 512, |_, m, _| sink += m[0]);
    }
    let per = t.elapsed().as_secs_f64() / n as f64;
    println!(
        "render 512 frames while scratching: {:.1} us per callback ({:.2}% of the 11.6 ms budget) {sink}",
        per * 1e6,
        per / (512.0 / 44100.0) * 100.0
    );
}

/// `rig` with stems: VOCAL is the whole track, the other three silent, so the VOCAL gain is the track's gain.
fn stem_rig(rate: f64, key_lock: bool) -> (Shared, Voice) {
    let (sh, v) = rig(rate, key_lock);
    let vocal: Vec<i16> = track(&sh).iter().map(|s| (s * 32768.0) as i16).collect(); // exact: the track is on a 2/32768 grid
    let n = vocal.len();
    *sh.stems.lock() = Some(Arc::new(Stems::complete([
        vocal,
        vec![0; n],
        vec![0; n],
        vec![0; n],
    ])));
    (sh, v)
}

fn set_stem(sh: &Shared, k: usize, g: f32) {
    sh.stem[k].store(g.to_bits(), Relaxed);
}

#[test]
fn stem_mute_ramps_in_10ms_without_a_click_and_unmute_returns() {
    let (sh, mut v) = stem_rig(1.0, false);
    let all_on = run_buffers(&sh, &mut v, 2);
    let src = track(&sh);
    assert!(
        all_on.iter().enumerate().all(|(i, &y)| y == src[2 * i]),
        "all stems on is the track itself"
    );
    set_stem(&sh, 0, 0.0);
    let out = run_buffers(&sh, &mut v, 2);
    let ramp = (STEM_RAMP_S * 44100.0) as usize + 2;
    assert!(
        out[..ramp / 2].iter().any(|y| y.abs() > 0.05),
        "fading, not muted instantly"
    );
    assert!(
        out[ramp..].iter().all(|y| y.abs() < 1e-3),
        "muted after the ramp"
    );
    assert!(
        out.windows(2).all(|w| (w[1] - w[0]).abs() < 0.06),
        "no step: a hard mute of a 0.5 sine would jump up to 0.5"
    );
    set_stem(&sh, 0, 1.0);
    let back = run_buffers(&sh, &mut v, 2);
    assert!(
        back[ramp..].iter().any(|y| y.abs() > 0.4)
            && back.windows(2).all(|w| (w[1] - w[0]).abs() < 0.06)
    );
}

#[test]
fn stems_share_the_read_position_with_key_lock_and_loops() {
    // Key lock at +10 %: pitch stays 440 Hz with every stem on, and the consumed source is the tempo's.
    let (sh, mut v) = stem_rig(1.1, true);
    let out = run_buffers(&sh, &mut v, 86);
    assert!(
        (pitch(&out) / 440.0 - 1.0).abs() < 0.01,
        "{:.1} Hz",
        pitch(&out)
    );
    assert!((f64::from_bits(sh.pos.load(Relaxed)) - 86.0 * 512.0 * 1.1).abs() < 1e-6);
    // Muting the stem that carries the sound silences key-locked playback (after the grain crossfade) and the position goes on.
    set_stem(&sh, 0, 0.0);
    let muted = run_buffers(&sh, &mut v, 8);
    assert!(
        muted[3000..].iter().all(|y| y.abs() < 1e-3),
        "key-locked playback follows the stem gains"
    );
    assert!(
        (f64::from_bits(sh.pos.load(Relaxed)) - 94.0 * 512.0 * 1.1).abs() < 1e-6,
        "muting does not move the playhead"
    );
    // A loop wraps the same way with stems: the playhead stays inside [1 s, 1.5 s).
    let (sh, mut v) = stem_rig(1.0, false);
    sh.set_loop(Some((1000.0, 1500.0)));
    sh.seek.store((44100.0f64).to_bits(), Relaxed);
    set_stem(&sh, 0, 0.5);
    run_buffers(&sh, &mut v, 200);
    let pos = f64::from_bits(sh.pos.load(Relaxed)) / 44100.0;
    assert!((1.0..1.5).contains(&pos), "{pos}");
}

#[test]
fn a_stem_set_of_the_wrong_length_is_ignored() {
    let (sh, mut v) = stem_rig(1.0, false);
    set_stem(&sh, 0, 0.0);
    // stale / mismatched: 1000 frames for a longer track; the audio callback must not mix it
    *sh.stems.lock() = Some(Arc::new(Stems::complete(std::array::from_fn(|_| {
        vec![0; 2000]
    }))));
    let out = run_buffers(&sh, &mut v, 2);
    assert!(out.iter().any(|y| y.abs() > 0.4), "plays the plain track");
}

/// `stem_rig` variant: VOCAL a steady 300 Hz tone, DRUMS a steady 1000 Hz tone (0.25 each), the track their sum.
fn fx_rig(key_lock: bool) -> (Shared, Voice) {
    let (sh, v) = rig(1.0, key_lock);
    // even i16 values: their sum is exact in the 16-bit track (steps of 2 / 32768)
    let tone = |hz: f64, i: usize| {
        (0.25 * (2.0 * std::f64::consts::PI * hz * i as f64 / 44100.0).sin() * 16384.0) as i16 * 2
    };
    let n = 44100 * 5;
    let (vocal, drums): (Vec<i16>, Vec<i16>) = (0..n)
        .flat_map(|i| [(tone(300.0, i), tone(1000.0, i)); 2])
        .unzip();
    set_track(
        &sh,
        Pcm {
            rate: 44100,
            samples: vocal
                .iter()
                .zip(&drums)
                .map(|(a, b)| (*a as f32 + *b as f32) / 32768.0)
                .collect(),
        },
    );
    *sh.stems.lock() = Some(Arc::new(Stems::complete([
        vocal,
        vec![0; 2 * n],
        vec![0; 2 * n],
        drums,
    ])));
    (sh, v)
}

const DRUMS_BIT: u8 = 1 << 3;

fn trans_on(level: f32) -> Settings {
    Settings {
        effect: fx::Effect::Trans.index(),
        beats: fx::BEATS[2],
        level,
        knob: 0.5,
        on: true,
        bpm: 120.0,
    } // 1/4 beat = 125 ms: 62 ms open, 62 ms shut
}

/// Like `run_buffers`, handing the voice the stem FX target before every buffer as the engine does.
fn run_part(sh: &Shared, v: &mut Voice, t: Option<(u8, Settings)>, buffers: usize) -> Vec<f32> {
    let mut out = vec![];
    if let Some(t) = t {
        v.part_fx.prepare(t.1.effect);
    }
    for _ in 0..buffers {
        v.set_part_fx(t);
        render(sh, v, 44100, 512, |_, m, _| out.push(m[0]));
    }
    out
}

/// Amplitude of the `hz` tone in each 441-frame (10 ms) block of `x`, with `hz` a multiple of 100.
fn blocks(x: &[f32], hz: f64) -> Vec<f32> {
    x.chunks_exact(441)
        .map(|b| {
            let w = 2.0 * std::f64::consts::PI * hz / 44100.0;
            let (s, c) = b.iter().enumerate().fold((0.0, 0.0), |(s, c), (i, y)| {
                (
                    s + *y as f64 * (w * i as f64).sin(),
                    c + *y as f64 * (w * i as f64).cos(),
                )
            });
            (2.0 / 441.0 * (s * s + c * c).sqrt()) as f32
        })
        .collect()
}

fn min_max(x: &[f32]) -> (f32, f32) {
    x.iter()
        .fold((f32::MAX, 0.0), |(lo, hi), &y| (lo.min(y), hi.max(y)))
}

#[test]
fn stem_fx_gates_only_the_chosen_stem_and_leaves_the_rest_and_other_decks_alone() {
    let (sh, mut v) = fx_rig(false);
    let (other, mut ov) = fx_rig(false);
    let out = run_part(&sh, &mut v, Some((DRUMS_BIT, trans_on(1.0))), 24);
    let plain = run_buffers(&other, &mut ov, 24); // another channel: no stem FX handed to it
    let (drums, vocal) = (
        min_max(&blocks(&out[2048..], 1000.0)),
        min_max(&blocks(&out[2048..], 300.0)),
    );
    assert!(drums.0 < 0.03 && drums.1 > 0.22, "drums gated: {drums:?}");
    assert!(
        vocal.0 > 0.23 && vocal.1 < 0.27,
        "vocal constant: {vocal:?}"
    );
    let od = min_max(&blocks(&plain[2048..], 1000.0));
    assert!(od.0 > 0.23, "other deck untouched: {od:?}");
}

#[test]
fn stem_fx_with_the_effect_at_level_0_equals_the_plain_mix() {
    let (a, mut va) = fx_rig(false);
    let (b, mut vb) = fx_rig(false);
    let fx = run_part(&a, &mut va, Some((DRUMS_BIT, trans_on(0.0))), 8);
    let plain = run_buffers(&b, &mut vb, 8);
    assert!(
        fx[256..]
            .iter()
            .zip(&plain[256..])
            .all(|(x, y)| (x - y).abs() < 1e-4),
        "rest + part = mix"
    );
}

#[test]
fn stem_fx_on_a_muted_part_has_nothing_to_gate() {
    let (sh, mut v) = fx_rig(false);
    set_stem(&sh, 3, 0.0);
    let out = run_part(&sh, &mut v, Some((DRUMS_BIT, trans_on(1.0))), 24);
    let (drums, vocal) = (
        min_max(&blocks(&out[2048..], 1000.0)),
        min_max(&blocks(&out[2048..], 300.0)),
    );
    assert!(drums.1 < 0.01, "drums are muted: {drums:?}");
    assert!(
        vocal.0 > 0.23 && vocal.1 < 0.27,
        "vocal is the rest: {vocal:?}"
    );
}

#[test]
fn stem_fx_without_stems_does_nothing_and_a_released_unit_idles() {
    let (sh, mut v) = rig(1.0, false);
    let out = run_part(&sh, &mut v, Some((DRUMS_BIT, trans_on(1.0))), 4);
    let src = track(&sh);
    assert!(
        out.iter().enumerate().all(|(i, &y)| y == src[2 * i]),
        "no stems: dry"
    );
    let (sh, mut v) = fx_rig(false);
    run_part(&sh, &mut v, Some((DRUMS_BIT, trans_on(1.0))), 4);
    run_part(&sh, &mut v, None, 20); // fades out in 5 ms, then one period + 50 ms of silence: idle
    assert!(v.part_fx.idle());
}

#[test]
fn stem_fx_with_key_lock_keeps_the_level_and_stays_finite() {
    let (a, mut va) = fx_rig(true);
    let (b, mut vb) = fx_rig(true);
    let fx = run_part(&a, &mut va, Some((DRUMS_BIT, trans_on(0.0))), 40);
    let plain = run_buffers(&b, &mut vb, 40);
    let rms =
        |x: &[f32]| (x[4096..].iter().map(|y| y * y).sum::<f32>() / (x.len() - 4096) as f32).sqrt();
    assert!(fx.iter().all(|y| y.is_finite()));
    assert!(
        (rms(&fx) / rms(&plain) - 1.0).abs() < 0.2,
        "{} vs {}",
        rms(&fx),
        rms(&plain)
    );
}

#[test]
fn play_from_stopped_spins_up_over_the_release_time() {
    let peak = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let (sh, mut v) = rig(1.0, false);
    sh.vinyl_release.store(0.1f32.to_bits(), Relaxed);
    sh.request_spin();
    let out = run_buffers(&sh, &mut v, 10); // 5120 frames, the ramp lasts 4410
    let pos = f64::from_bits(sh.pos.load(Relaxed));
    assert!(
        (2800.0..3000.0).contains(&pos),
        "half the distance during the ramp plus 710 frames at full speed: {pos}"
    );
    assert!(
        peak(&out[..30]) < 0.05,
        "the first frames are nearly silent (speed ~ 0)"
    );
    assert!(peak(&out[4600..]) > 0.4, "full volume once at speed");
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 10);
    assert_eq!(
        f64::from_bits(sh.pos.load(Relaxed)),
        5120.0,
        "without the setting the speed is constant"
    );
}

#[test]
fn the_platter_let_go_of_a_playing_deck_spins_it_up_but_not_a_paused_one() {
    let (sh, mut v) = rig(1.0, false);
    sh.vinyl_release.store(0.1f32.to_bits(), Relaxed);
    sh.scratch_begin();
    run_buffers(&sh, &mut v, 2);
    sh.scratch_end();
    assert!(
        f32::from_bits(sh.spin.load(Relaxed)) > 0.0,
        "a playing deck asks for the speed-up"
    );
    let (sh, _) = rig(1.0, false);
    sh.vinyl_release.store(0.1f32.to_bits(), Relaxed);
    sh.playing.store(false, Relaxed);
    sh.scratch_begin();
    sh.scratch_end();
    assert_eq!(sh.spin.load(Relaxed), 0, "a paused deck just stays put");
}

#[test]
fn external_mixer_mode_plays_the_track_raw_without_the_fader_gain() {
    let peak = |raw: bool| {
        let (sh, mut v) = rig(1.0, false);
        for a in [&sh.gain, &sh.gain_applied] {
            a.store(0.5f32.to_bits(), Relaxed);
        }
        sh.raw.store(raw, Relaxed);
        run_buffers(&sh, &mut v, 4)
            .iter()
            .fold(0f32, |m, y| m.max(y.abs()))
    };
    assert!((peak(false) - 0.25).abs() < 0.01 && (peak(true) - 0.5).abs() < 0.01);
}

/// `fx_rig` (VOCAL 300 Hz, DRUMS 1000 Hz, 0.25 each) on a 120 BPM grid from 0 with a 4-beat 600 Hz drum loop (0.4) in slot 0, pressed.
fn swap_rig(multi: bool) -> (Shared, Voice) {
    let (sh, v) = fx_rig(false);
    *sh.beats.lock() = Some((0..40).map(|i| 500 * i).collect::<Vec<u32>>().into());
    let tone = (0..4 * 22050)
        .flat_map(|i| {
            [(0.4 * (2.0 * std::f64::consts::PI * 600.0 * i as f64 / 44100.0).sin()) as f32; 2]
        })
        .collect();
    sh.groove.set_multi(multi);
    sh.groove.load(
        0,
        Some(
            groove::Drum::new(
                Pcm {
                    rate: 44100,
                    samples: tone,
                },
                120.0,
            )
            .unwrap(),
        ),
    );
    sh.groove.press(0);
    (sh, v)
}

/// The amplitudes of the VOCAL (300 Hz), loop (600 Hz) and DRUMS (1000 Hz) tones in 10 ms block `b`.
fn tones(x: &[f32], b: usize) -> [f32; 3] {
    let x = &x[b * 441..(b + 1) * 441];
    [300.0, 600.0, 1000.0].map(|hz| blocks(x, hz)[0])
}

#[test]
fn drum_swap_replaces_the_drums_stem_on_the_beat_and_gives_it_back_when_stopped() {
    let (sh, mut v) = swap_rig(false);
    let x = run_buffers(&sh, &mut v, 103); // 1.2 s: the loop starts at beat 1 = 22050 frames = block 50
    let before = tones(&x, 40);
    assert!(
        before[2] > 0.2 && before[1] < 0.01,
        "the drums play until the beat: {before:?}"
    );
    let during = tones(&x, 80);
    assert!(
        during[2] < 0.01 && (during[1] - 0.4).abs() < 0.03 && (during[0] - 0.25).abs() < 0.02,
        "loop in, drums out, vocal kept: {during:?}"
    );
    sh.groove.press(0);
    let y = run_buffers(&sh, &mut v, 20);
    let after = tones(&y, 20);
    assert!(
        after[2] > 0.2 && after[1] < 0.01 && (after[0] - 0.25).abs() < 0.02,
        "the drums are back: {after:?}"
    );
    assert!(!sh.groove.swapping());
    assert!(
        x.iter().chain(&y).all(|s| s.abs() < 1.0),
        "no summing blow-up"
    );
}

#[test]
fn drum_swap_multi_mode_adds_the_loop_to_the_drums() {
    let (sh, mut v) = swap_rig(true);
    let x = run_buffers(&sh, &mut v, 103);
    let t = tones(&x, 80);
    assert!(t[2] > 0.2 && (t[1] - 0.4).abs() < 0.03, "both play: {t:?}");
}

#[test]
fn drum_swap_stops_with_the_deck_and_does_not_play_on_a_stopped_one() {
    let (sh, mut v) = swap_rig(false);
    run_buffers(&sh, &mut v, 80);
    assert!(sh.groove.swapping());
    sh.playing.store(false, Relaxed);
    run_buffers(&sh, &mut v, 2);
    assert!(
        !sh.groove.swapping() && sh.groove.state(0) == groove::State::Idle,
        "pause stops the loop"
    );
    sh.groove.press(0);
    run_buffers(&sh, &mut v, 2); // a press while the deck is stopped is ignored
    sh.playing.store(true, Relaxed);
    let x = run_buffers(&sh, &mut v, 60); // past the next beat
    assert!(sh.groove.state(0) == groove::State::Idle && !sh.groove.swapping());
    assert!(
        x.iter().all(|s| s.abs() < 0.6),
        "only the track: its two 0.25 tones"
    );
}

#[test]
fn drum_release_fx_gates_the_stem_drums_when_held_and_in_multi_mode() {
    let (sh, mut v) = swap_rig(true); // MULTI: the effect is for the DRUMS stem only, the loop plays on
    sh.groove.release.set_effect(fx::Effect::Trans);
    sh.groove.release.set_beat(2); // 1/4
    sh.groove.release.set_level(1.0);
    sh.groove.release.set_on(true);
    let mut out = vec![];
    v.part_fx.prepare(fx::Effect::Trans.index());
    for _ in 0..103 {
        v.set_part_fx(sh.groove.release_target(0));
        render(&sh, &mut v, 44100, 512, |_, m, _| out.push(m[0]));
    }
    let drums: Vec<f32> = blocks(&out[30_000..], 1000.0);
    let (lo, hi) = min_max(&drums);
    assert!(
        lo < 0.05 && hi > 0.2,
        "the stem drums are gated: {lo} .. {hi}"
    );
    assert!(
        blocks(&out[30_000..], 600.0)
            .iter()
            .all(|a| (a - 0.4).abs() < 0.05),
        "the loop is not"
    );
}

#[test]
fn a_fader_plugin_replaces_the_fader_gain_on_the_master_path_and_never_touches_the_cue_path() {
    let (sh, _) = rig(1.0, false);
    let plugins = crate::plugin::effect::fake_effects();
    let mut v = Voice::with_faders(44100, &plugins[..1]); // GAIN: doubles the signal
    sh.cue.store(1f32.to_bits(), Relaxed);
    sh.cue_applied.store(1f32.to_bits(), Relaxed);
    let src: Vec<f32> = track(&sh).into_iter().step_by(2).collect();
    let mut run = |buffers: usize| {
        let mut out = Vec::with_capacity(buffers * 512);
        for _ in 0..buffers {
            render(&sh, &mut v, 44100, 512, |_, m, c| out.push((m[0], c[0])));
        }
        out
    };
    // No plugin curve selected: the plain channel (master gain 1 here).
    let plain = run(2);
    assert!(plain.iter().zip(&src).all(|(&(m, c), &s)| m == s && c == s));
    // Selected: the master path is the plugin's output, the cue path is still the channel.
    sh.fader.set_index(0);
    sh.fader.set_knob(1.0);
    sh.fader.set_on(true);
    let on = run(4);
    let at = plain.len();
    assert!(
        on.iter()
            .zip(&src[at..])
            .skip(512)
            .all(|(&(m, c), &s)| (m - 2.0 * s).abs() < 1e-6 && c == s)
    );
    // Deselected: it fades out, rings out and the master path is bit-exact again.
    sh.fader.set_on(false);
    let off = run(60);
    let at = at + on.len();
    assert!(
        off[off.len() - 512..]
            .iter()
            .zip(&src[at + off.len() - 512..])
            .all(|(&(m, _), &s)| m == s)
    );
}

#[test]
fn steady_is_off_only_while_a_brake_or_spin_up_changes_the_speed() {
    let (sh, mut v) = rig(1.0, false);
    run_buffers(&sh, &mut v, 2);
    assert!(sh.steady.load(Relaxed), "plain play");
    sh.t.brake(0.2); // 8820 frames = 17 buffers
    run_buffers(&sh, &mut v, 2);
    assert!(!sh.steady.load(Relaxed), "braking");
    run_buffers(&sh, &mut v, 40);
    assert!(!sh.playing.load(Relaxed) && sh.steady.load(Relaxed), "stopped");
    sh.vinyl_release.store(0.1f32.to_bits(), Relaxed); // 4410 frames = 9 buffers
    sh.request_spin();
    sh.playing.store(true, Relaxed);
    run_buffers(&sh, &mut v, 2);
    assert!(!sh.steady.load(Relaxed), "spinning up");
    run_buffers(&sh, &mut v, 12);
    assert!(sh.steady.load(Relaxed), "at speed");
}

#[test]
fn a_deck_at_tempo_zero_is_silent_not_one_held_sample() {
    for key_lock in [false, true] {
        let (sh, mut v) = rig(1.0, key_lock);
        run_buffers(&sh, &mut v, 40);
        sh.rate.store(0f64.to_bits(), Relaxed); // the bottom of the WIDE tempo range
        let out = run_buffers(&sh, &mut v, 40);
        let peak = out[out.len() - 4096..].iter().fold(0f32, |m, y| m.max(y.abs()));
        assert!(peak < 1e-3, "key lock {key_lock}: peak {peak}");
    }
}
