//! A hot cue pressed again and again while the deck plays, through the audio callback and the position clock (virtual time,
//! so it is deterministic): where does each press land? Without Q on the cue; with Q on the cue plus the beat phase of the frames
//! the deck writes when the jump takes effect, never a beat off whatever the press time, the buffer and the output latency.

use super::*;
use std::time::Duration;

const RATE: f64 = 44100.0;
const BUFFER: usize = 512;
/// 120 BPM: a beat is 22050 frames, the grid starts at frame 0.
const BEAT: f64 = 22050.0;
/// The hot cue: beat 8 (4 s).
const CUE: f64 = 8.0 * BEAT;
const FRAMES: usize = 6 * 44100;
/// A stored loop's length: shorter than a beat, so its phase wraps.
const LOOP: f64 = 0.4 * BEAT;

/// The landing's offset from the cue: the beat phase of the written frame `w`, wrapped in `wrap` (> 0).
fn phase(w: f64, wrap: f64) -> f64 {
    let p = w.rem_euclid(BEAT);
    if wrap > 0.0 {
        p % wrap
    } else {
        p
    }
}

/// Frame `i` as a stereo sample: its number in 14 bits per channel, so any output frame says where in the track it came from.
fn code(i: usize) -> [f32; 2] {
    [
        (i & 0x3FFF) as f32 / 16384.0,
        (i >> 14 & 0x3FFF) as f32 / 16384.0,
    ]
}

fn frame_of(f: [f32; 2]) -> usize {
    (f[0] * 16384.0).round() as usize | ((f[1] * 16384.0).round() as usize) << 14
}

/// One deck playing a coded track, rendered buffer by buffer at the times the device would call.
struct Deck {
    sh: Shared,
    v: Voice,
    t0: Instant,
    buffers: u32,
    latency: Duration,
}

impl Deck {
    fn new(key_lock: bool, rate: f64, latency: Duration) -> Deck {
        let sh = Shared::new();
        let samples = (0..FRAMES).flat_map(code).collect();
        *sh.pcm.lock() = Arc::new(Track16::from(Pcm {
            rate: 44100,
            samples,
        }));
        sh.playing.store(true, Relaxed);
        sh.rate.store(rate.to_bits(), Relaxed);
        sh.key_lock.store(key_lock, Relaxed);
        sh.seek_to(CUE - 4.0 * BEAT);
        let mut d = Deck {
            sh,
            v: Voice::new(44100),
            t0: Instant::now(),
            buffers: 0,
            latency,
        };
        for _ in 0..8 {
            d.callback();
        }
        d
    }

    fn at(&self, buffer: u32) -> Instant {
        self.t0 + Duration::from_secs_f64(buffer as f64 * BUFFER as f64 / RATE)
    }

    /// One device callback: the buffer's frames and the frame it started at, the anchor the clock keeps as `Mix::fill` records it.
    fn callback(&mut self) -> (Vec<[f32; 2]>, f64) {
        let mut out = Vec::with_capacity(BUFFER);
        let d = render(&self.sh, &mut self.v, 44100, BUFFER, |_, m, _| out.push(m));
        let mut clock = self.sh.clock.lock();
        let reset = d.seeked || !clock.playing;
        let a = Anchor {
            at: self.at(self.buffers),
            pos: d.start,
            latency: self.latency,
            fps: d.fps,
            lp: d.lp,
        };
        clock.push(a, d.playing, reset);
        clock.epoch = d.epoch;
        self.buffers += 1;
        (out, d.start)
    }

    /// The frame the next buffer starts at.
    fn written(&self) -> f64 {
        f64::from_bits(self.sh.pos.load(Relaxed))
    }

    /// What `Player::position_ms` reports `after` the last callback, in source frames.
    fn heard(&self, after: Duration) -> f64 {
        let seek = *self.sh.seek.lock();
        let clock = self.sh.clock.lock();
        if seek.epoch != clock.epoch {
            return seek.target;
        }
        let now = self.at(self.buffers - 1) + after;
        heard_frames(&clock, now, FRAMES as f64)
    }

    /// The press of a hot cue pad `after` the last callback: without Q a plain seek; with Q the grid's beat that the heard playhead is in
    /// (as the app's `quantize::window` finds it) handed to the audio thread, which takes the phase itself.
    fn press(&self, quantize: bool, after: Duration, wrap: f64) {
        if quantize {
            let beat_start = (self.heard(after) / BEAT).floor() * BEAT;
            self.sh.seek_on_beat(CUE, beat_start, BEAT, wrap);
        } else {
            self.sh.seek_to(CUE);
        }
        self.sh.play(false);
    }
}

/// A small deterministic random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// 600 presses at every offset into the buffer and into the beat: each must land at the cue, or with Q at the cue plus the beat
/// phase of the frames the deck writes when the jump takes effect (exactly, whatever the offsets), and play on from there.
fn spam(key_lock: bool, rate: f64, quantize: bool, latency_ms: u64, wrap: f64) {
    let mut d = Deck::new(key_lock, rate, Duration::from_millis(latency_ms));
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut wrong = vec![];
    for n in 0..600 {
        for _ in 0..1 + (rng.next() * 48.0) as usize {
            d.callback();
        }
        d.press(
            quantize,
            Duration::from_secs_f64(rng.next() * BUFFER as f64 / RATE),
            wrap,
        );
        let w = d.written();
        let (out, start) = d.callback();
        let expected = CUE + if quantize { phase(w, wrap) } else { 0.0 };
        if start != expected {
            wrong.push((n, expected, start));
        }
        if rate == 1.0 {
            // every frame of the buffer comes from the landing on (a key-locked buffer is exact for its first grain)
            let run = if key_lock { 64 } else { BUFFER };
            assert!(
                (0..run).all(|i| out[i] == code(expected as usize + i)),
                "press {n}: the buffer plays on from frame {expected}, not {}",
                frame_of(out[0])
            );
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of 600 presses landed off (press, expected, got): {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)]
    );
}

#[test]
fn hot_cue_spam_without_quantize_lands_on_the_cue() {
    for key_lock in [false, true] {
        for rate in [1.0, 1.08] {
            spam(key_lock, rate, false, 30, 0.0);
        }
    }
}

#[test]
fn hot_cue_spam_with_quantize_keeps_the_beat_of_the_jump_at_any_latency_key_lock_and_tempo() {
    for key_lock in [false, true] {
        for rate in [1.0, 0.93] {
            for latency_ms in [12, 85] {
                spam(key_lock, rate, true, latency_ms, 0.0);
            }
        }
    }
}

/// The reason the phase is not worked out when the pad is pressed: with the press's own phase (heard just before a beat) and the
/// catch-up of what plays until the jump takes effect, a beat boundary in between lands the cue a whole beat late.
#[test]
fn a_phase_taken_at_the_press_lands_a_beat_late_when_a_beat_comes_before_the_jump() {
    let mut d = Deck::new(false, 1.0, Duration::from_millis(30));
    d.sh.seek_to(10.0 * BEAT + 700.0 - BUFFER as f64);
    d.callback(); // written: 700 frames past beat 10
    let heard = d.written() - 1000.0; // 300 frames before it
    let phase = heard.rem_euclid(BEAT);
    assert_eq!(phase, BEAT - 300.0);
    d.sh.seek_from_heard(CUE + phase, Some(heard));
    let w = d.written();
    let (_, start) = d.callback();
    assert_eq!(start, CUE + w.rem_euclid(BEAT) + BEAT, "a beat late");
    d.sh.seek_on_beat(CUE, 0.0, BEAT, 0.0);
    let w = d.written();
    let (_, start) = d.callback();
    assert_eq!(start, CUE + w.rem_euclid(BEAT), "on the beat");
}

/// Two presses before the callback takes either: the last one counts whole, and a plain seek after a beat seek is not moved.
#[test]
fn the_last_of_two_presses_before_a_buffer_decides() {
    let mut d = Deck::new(false, 1.0, Duration::from_millis(30));
    d.sh.seek_on_beat(CUE + 3.0 * BEAT, 0.0, BEAT, 0.0);
    d.press(true, Duration::ZERO, 0.0);
    let w = d.written();
    assert_eq!(d.callback().1, CUE + w.rem_euclid(BEAT));
    d.sh.seek_on_beat(CUE, 0.0, BEAT, 0.0);
    d.sh.seek_to(CUE + 3.0 * BEAT);
    assert_eq!(d.callback().1, CUE + 3.0 * BEAT);
    d.sh.seek_to(CUE);
    d.sh.seek_on_beat(CUE + BEAT, 0.0, BEAT, 0.0);
    let w = d.written();
    assert_eq!(d.callback().1, CUE + BEAT + w.rem_euclid(BEAT));
}

/// A grid that does not start at frame 0, and a reversed deck.
#[test]
fn the_beat_grid_may_start_anywhere_and_the_deck_may_run_backwards() {
    let mut d = Deck::new(false, 1.0, Duration::from_millis(30));
    let start = 1234.5; // the grid's beats sit 1234.5 frames past multiples of BEAT
    d.sh.seek_on_beat(CUE + start, start + 5.0 * BEAT, BEAT, 0.0);
    let w = d.written();
    assert_eq!(d.callback().1, CUE + start + (w - start).rem_euclid(BEAT));
    d.sh.set_reverse(true);
    d.sh.seek_on_beat(CUE, 0.0, BEAT, 0.0);
    let w = d.written();
    assert_eq!(d.callback().1, CUE + w.rem_euclid(BEAT));
}

/// A stored loop called with Q on (the loop is `LOOP` frames long, shorter than a beat): the landing is the loop's start plus the
/// beat phase of the written frames, wrapped round the loop, at every press time across beat boundaries.
#[test]
fn loop_pad_presses_across_beat_boundaries_land_inside_the_loop_on_the_written_phase() {
    for latency_ms in [12, 85] {
        spam(false, 1.0, true, latency_ms, LOOP);
        spam(true, 0.93, true, latency_ms, LOOP);
    }
}

/// Slip on: a pad released and pressed again before the callback runs. The release asks for the return to the shadow, the next
/// press (a plain seek, or the beat's) must still win; that press's own release then returns to the shadow.
#[test]
fn a_press_right_after_a_release_in_the_same_buffer_beats_the_slip_return() {
    for (quantize, wrap) in [(false, 0.0), (true, 0.0), (true, LOOP)] {
        let mut d = Deck::new(false, 1.0, Duration::from_millis(30));
        d.sh.t.set_slip(true);
        d.callback();
        for n in 0..40 {
            d.press(quantize, Duration::ZERO, wrap); // press, heard for a while
            d.callback();
            d.callback();
            d.sh.t.slip_return(); // released ...
            d.press(quantize, Duration::ZERO, wrap); // ... and pressed again before the callback
            let w = d.written();
            let want = CUE + if quantize { phase(w, wrap) } else { 0.0 };
            assert_eq!(d.callback().1, want, "press {n}: the new jump wins");
        }
        // the last press is let go: back on the shadow, which ran on while the deck was jumping about
        let shadow = d.sh.t.shadow().unwrap();
        d.sh.t.slip_return();
        assert_eq!(
            d.callback().1,
            shadow,
            "its own release returns to the shadow"
        );
    }
}

fn real_player() -> (Player, Voice) {
    let shared = [(); engine::SLOTS].map(|_| Arc::new(Shared::new()));
    *shared[0].pcm.lock() = Arc::new(Track16::from(Pcm {
        rate: 44100,
        samples: vec![0.25; 2 * FRAMES],
    }));
    let player = engine::test_player(shared);
    (player, Voice::new(44100))
}

fn publish(sh: &Shared, d: &Done) {
    let mut clock = sh.clock.lock();
    let reset = d.seeked || !clock.playing;
    clock.push(
        Anchor {
            at: Instant::now(),
            pos: d.start,
            latency: Duration::from_millis(200),
            fps: d.fps,
            lp: d.lp,
        },
        d.playing,
        reset,
    );
    clock.epoch = d.epoch;
}

#[test]
fn cue_targets_survive_consumption_and_older_callbacks_cannot_acknowledge_new_presses() {
    let (mut player, mut voice) = real_player();
    let sh = player.shared.clone();
    player.seek(3000);
    player.play();
    let old = render(&sh, &mut voice, 44100, BUFFER, |_, _, _| {});
    publish(&sh, &old);
    for press in 0..64 {
        let target = 1000 + (press % 2) * 1000;
        player.pause();
        player.seek(target);
        player.play();
        let mut latest = target;
        let resume = press % 2 == 0;
        let d = render(&sh, &mut voice, 44100, BUFFER, |i, _, _| {
            if i != 0 {
                return;
            }
            assert_eq!(
                player.position_ms(),
                target as f64,
                "consumed, not published"
            );
            assert_eq!(player.written_ms(), target as f64);
            assert!(!player.settled());
            // Release and press again while the earlier preview is still rendering.
            player.pause();
            latest = if press % 3 == 0 { target + 500 } else { target };
            player.seek(latest);
            if resume {
                player.play();
            }
            assert_eq!(player.position_ms(), latest as f64);
            assert_eq!(player.written_ms(), latest as f64);
        });
        assert_eq!(d.start, target as f64 * RATE / 1000.0);
        assert_eq!(
            player.position_ms(),
            latest as f64,
            "before clock publication"
        );
        assert_eq!(
            player.written_ms(),
            latest as f64,
            "older position publication"
        );
        publish(&sh, &d);
        assert_eq!(
            player.position_ms(),
            latest as f64,
            "older clock publication"
        );
        assert!(
            !player.settled(),
            "even an identical target needs its own epoch"
        );
        let d = render(&sh, &mut voice, 44100, BUFFER, |_, _, _| {});
        assert_eq!(d.start, latest as f64 * RATE / 1000.0);
        assert_eq!(player.position_ms(), latest as f64);
        publish(&sh, &d);
        assert_eq!(player.position_ms(), latest as f64);
        let epoch = sh.clock.lock().epoch;
        assert_eq!(epoch, sh.seek.lock().epoch);
        assert_eq!(player.settled(), !resume);
    }
}

#[test]
fn a_busy_seek_mailbox_defers_the_whole_command_without_blocking_render() {
    let (mut player, mut voice) = real_player();
    let sh = player.shared.clone();
    player.play();
    player.seek_on_beat(4000, 0.0, 500.0, 200.0);
    let held = sh.seek.lock();
    let d = render(&sh, &mut voice, 44100, BUFFER, |_, _, _| {});
    assert!(!d.seeked);
    assert!(held.pending, "a partial publication is never consumed");
    drop(held);
    assert_eq!(player.position_ms(), 4000.0);
    let written = f64::from_bits(sh.pos.load(Relaxed));
    let d = render(&sh, &mut voice, 44100, BUFFER, |i, _, _| {
        if i == 0 {
            player.seek(2000); // the next command must not inherit the beat or wrap
        }
    });
    assert_eq!(d.start, CUE + phase(written, 0.4 * BEAT));
    publish(&sh, &d);
    assert_eq!(player.position_ms(), 2000.0);
    let d = render(&sh, &mut voice, 44100, BUFFER, |_, _, _| {});
    assert_eq!(d.start, 2.0 * RATE);
    publish(&sh, &d);
    assert_eq!(player.position_ms(), 2000.0);
}
