//! One mixer channel's audio-thread state and [`render`].
//!
//! Varispeed: source frames advance by `rate * src_rate / dev_rate` per output frame, so frames
//! heard per second = `rate * src_rate` (windowed-sinc interpolation; pitch follows rate). With key lock
//! ([`super::super::stretch`]) the output runs at 1:1 pitch while this same `advance` clock still moves at
//! `rate`; it reports the nominal position, not the overlapping grains' individual source positions. A key shift
//! ([`super::super::transport`]) rides the same stretcher at `pitch` times its read step, so tempo (the clock)
//! stays put. Reverse flips the sign of the step (the clock's `fps` goes negative; playback at 0 pauses),
//! brake scales it by a factor falling to 0 and pauses; both stay on `advance`, so loops and the clock keep
//! working. Switching between the varispeed and stretched paths fades the output in over [`FADE`] frames.
//! Loop: [`advance`] wraps at the end *only when crossing it from below* (at the start when moving
//! backward). `Player::seek` does not touch the loop; a seek past the loop end plays on and ignores it, a
//! seek before it enters it at the start. (Inferred.) A loop that shrinks under the playhead
//! (halved, a shorter beat loop or SLICER length) keeps it inside ([`shrunk`]). `Player::load` clears the
//! loop (ms are per track). Scratch ([`super::scratch`]) replaces `advance` while the jog is held: the playhead
//! follows the platter target, audio is read at it (varispeed, key lock / shift / reverse / brake bypassed,
//! loop ignored); the transport state is untouched, so release resumes it, and the slip shadow keeps running. Nudge
//! adds to the rate (audio and clock) but not to the shadow's tempo. Stems ([`super::super::stems`]) are read at the
//! same position as the track: while any stem gain is off 1 the source is the track with the gains applied (ramped per
//! output frame over [`STEM_RAMP_S`], so mute/solo is click-free; the stretcher takes them per grain), else the plain track.
//!
//! Stem FX ([`super::super::fx`]): the voice owns one Beat FX [`Unit`] for the stems the engine hands it ([`Voice::set_part_fx`];
//! one per channel, ponytail: if FX1 and FX2 both target parts of this channel the engine gives it FX1 and FX2 stays silent
//! here; per-unit part state in the voice if that matters). While the unit is not idle and the track has stems, the voice reads
//! `part` (the chosen stems at their gains) and `rest` (the mix minus `part`) at the same position, each through its own
//! stretcher when key lock / shift is on (they may differ in phase by less than the stretcher's tolerance), and outputs
//! `rest + unit(part)` before the EQ, COLOR FX and fader, so those, the cue path, JOG cutter, scratch and brake treat both alike
//! and the FX tail passes through the fader. With the effect off the sum equals the plain mix. Paused or at the end of the
//! track the unit is fed zeros so its tail rings out; without stems it does nothing. The path switch fades in over [`FADE`].
//!
//! FADER plugin curve ([`super::super::fx::Unit::rack`], slot Fader): while one is selected ([`Shared::fader`]) the voice runs it on
//! the master path after COLOR FX and before the ramped gain; the effect is the fader (it applies the gain for the raw fader
//! position), so the app's channel gain then holds trim and crossfader only. The cue path never sees it. An effect that is off
//! fades out over the unit's 5 ms ramp and must return its input at `g` 0 (as `Ctx::mix` does); once the unit is idle it is skipped.

use super::super::cfx::Cfx;
use super::super::dsp::Fx;
use super::super::fx::{Settings, Unit};
use super::super::groove::{self, Link};
use super::super::stems::{COUNT, DRUMS, Mixed, Part, Rest, Source};
use super::super::stretch::Stretch;
use super::super::transport::{self, Brake};
use super::super::{Loop, Track16, advance, shrunk};
use super::scratch::{GATE_SPEED, Scratch, Spin};
use super::{NO_SEEK, Shared, gain_at, load_f32, step, unpack};
use crate::plugin::{
    effect::{Effect as PluginEffect, Slot},
    registry,
};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// Fade-in (output frames) when the source path switches between varispeed and the stretcher.
const FADE: u32 = 128;
/// Stem gain ramp, seconds (full scale).
pub(super) const STEM_RAMP_S: f32 = 0.01;
/// JOG CUTTER gate ramp, seconds.
const CUT_S: f32 = 0.001;
/// Speed factor (relative to normal) at and above which a spin-up is at full volume; below it the gain follows the speed.
const SPIN_GATE: f32 = GATE_SPEED as f32;
/// Largest catch-up of a phase-kept seek, source frames (about 2 s): latency and a buffer wait stay far below it.
const CATCH_UP: f64 = 96_000.0;
/// How far (seconds of track) past the end of a newly set loop the written playhead may be and still be wrapped into it:
/// the output latency plus a buffer, which a loop worked out from the heard playhead can trail by.
const LOOP_LAG_S: f64 = 0.5;

/// Audio-thread-only state, allocated before the stream starts.
pub(super) struct Voice {
    fx: Fx,
    cfx: Cfx,
    /// Selected FADER plugin curve ([`Shared::fader`] selects).
    fader: Unit,
    scr: Scratch,
    spin: Spin,
    stretch: Stretch,
    brake: Brake,
    /// Stem gains as heard (each ramps toward `Shared::stem`); all 1 while the track has no stems.
    sg: [f32; COUNT],
    stretched: bool,
    /// Fade-in frames left after a path switch.
    fade: u32,
    /// JOG CUTTER gate gain, 1 open .. 0 cut.
    cut: f32,
    /// Stem FX: the unit, the stems it works on (bit k = stem k), the second stretcher of the split path and whether it ran.
    pub(super) part_fx: Unit,
    mask: u8,
    part_stretch: Stretch,
    split: bool,
    /// BEAT LAB: this deck's DRUM SWAP loops, added to the track before the EQ ([`groove`]).
    groove: groove::Voice,
    /// The loop of the last buffer that played, to see it shrink under the playhead.
    lp_was: Loop,
}

impl Voice {
    pub(super) fn new(rate: u32) -> Voice {
        Voice::with_faders(rate, registry().slot(Slot::Fader))
    }

    /// A voice whose FADER rack holds `faders`.
    pub(super) fn with_faders(rate: u32, faders: &[PluginEffect]) -> Voice {
        Voice {
            fx: Fx::new(),
            cfx: Cfx::new(rate),
            fader: Unit::rack_with(rate, faders),
            scr: Scratch::new(),
            spin: Spin::new(),
            stretch: Stretch::new(rate),
            sg: [1.0; COUNT],
            brake: Brake::new(),
            stretched: false,
            fade: 0,
            cut: 1.0,
            part_fx: Unit::new(rate),
            mask: 0,
            part_stretch: Stretch::new(rate),
            split: false,
            groove: groove::Voice::new(rate),
            lp_was: None,
        }
    }

    /// The stem FX for this channel for the next buffer: `Some((mask, settings))` while a Beat FX unit targets stems of it, else the
    /// unit is released (it fades out, its trail rings out, then it idles). No allocation.
    pub(super) fn set_part_fx(&mut self, t: Option<(u8, Settings)>) {
        match t {
            Some((m, s)) => {
                self.mask = m;
                self.part_fx.set(s);
            }
            None => self.part_fx.release(),
        }
    }
}

/// What `render` leaves for the clock.
pub(super) struct Done {
    pub(super) seeked: bool,
    pub(super) epoch: u64,
    pub(super) start: f64,
    pub(super) playing: bool,
    pub(super) fps: f64,
    pub(super) lp: Loop,
}

/// The source frame heard at `pos`: a varispeed read at `vel` source frames per output frame (the interpolator low-passes
/// when that is above 1), or with `stretch` the next WSOLA frame (`None` past the end).
fn read(
    src: &impl Source,
    stretch: Option<&mut Stretch>,
    pos: f64,
    frames: f64,
    (kp, vel): (f64, f64),
    lp: Loop,
) -> Option<[f32; 2]> {
    match stretch {
        None => src.frame_at_step(pos, vel),
        Some(s) if pos < frames => Some(s.next(src, pos, kp, lp)),
        Some(_) => None,
    }
}

/// Render `n` frames, handing each stereo frame to `put(i, master, cue)`: source (varispeed, or WSOLA
/// for key lock / key shift) -> EQ -> filter or COLOR FX ([`super::super::cfx`]) -> gain (`master`) and -> cue gain (`cue`, pre-fader).
/// Does not allocate or block.
pub(super) fn render(
    sh: &Shared,
    v: &mut Voice,
    dev_rate: u32,
    n: usize,
    mut put: impl FnMut(usize, [f32; 2], [f32; 2]),
) -> Done {
    let t = &sh.t;
    let mut epoch = sh.pos_epoch.load(Acquire);
    let seek = sh.seek.try_lock().and_then(|mut seek| {
        if !seek.pending {
            return None;
        }
        seek.pending = false;
        Some(*seek)
    });
    if let Some(seek) = seek {
        epoch = seek.epoch;
        // A phase-kept seek moves on by what the deck played since the press was heard (latency + the wait for this buffer).
        // Forward only and within CATCH_UP: anything else (reverse, an older press time) does not apply. Inside the loop of the last
        // buffer that is the way round it, wrapped or not (a lap shorter than the latency is lost); the loop may be gone by now.
        // A seek on the beat lands at its target plus the phase of the frame this buffer continues from: the same beat phase the
        // listener hears the jump come in at, however long the press, the buffer and the output latency took.
        let from = seek.from;
        let beat = super::unpack_beat(seek.beat);
        let wrap = seek.wrap as f64;
        let (written, heard) = (f64::from_bits(sh.pos.load(Relaxed)), f64::from_bits(from));
        let played = match v.lp_was {
            Some((ls, le)) if (ls..le).contains(&written) && (ls..le).contains(&heard) => {
                (written - heard).rem_euclid(le - ls)
            }
            _ => written - heard,
        };
        let s = match beat {
            Some((start, len)) => {
                let phase = (written - start).rem_euclid(len);
                (seek.target + if wrap > 0.0 { phase % wrap } else { phase }).to_bits()
            }
            None if from != NO_SEEK && (0.0..CATCH_UP).contains(&played) && !sh.t.reverse() => {
                (seek.target + played).to_bits()
            }
            None => seek.target.to_bits(),
        };
        sh.pos.store(s, Relaxed);
        v.stretch.reset();
        v.part_stretch.reset();
    }
    let mut start = f64::from_bits(sh.pos.load(Relaxed));
    let base = f64::from_bits(sh.rate.load(Relaxed));
    let rate = base + load_f32(&sh.nudge) as f64;
    let (g0, g1) = (load_f32(&sh.gain_applied), load_f32(&sh.gain));
    sh.gain_applied.store(g1.to_bits(), Relaxed);
    let (c0, c1) = (load_f32(&sh.cue_applied), load_f32(&sh.cue));
    sh.cue_applied.store(c1.to_bits(), Relaxed);
    // External mixer mode: the track as it is, no fader / crossfader gain (EQ, filter and COLOR FX are skipped below).
    let raw = sh.raw.load(Relaxed);
    let (g0, g1) = if raw { (1.0, 1.0) } else { (g0, g1) };
    // try_lock: a concurrent load() yields one buffer of silence instead of blocking the audio thread
    let (transport, scratching) = (sh.playing.load(Relaxed), sh.scratch.load(Acquire));
    let guard = if transport || scratching {
        sh.pcm.try_lock()
    } else {
        None
    };
    // The stems of this track, if separated: skipped for one buffer while `set_stems`/`load` swap them.
    let stems_guard = guard.as_ref().and_then(|_| sh.stems.try_lock());
    let playing = guard.is_some();
    let src_rate = guard.as_ref().map_or(0.0, |p| p.rate as f64);
    let frames = guard.as_ref().map_or(0.0, |p| p.frames() as f64);
    let stems = stems_guard
        .as_ref()
        .and_then(|g| g.as_ref())
        .filter(|s| s.frames() as f64 == frames);
    let mut stem_to = sh.stem.each_ref().map(load_f32);
    if sh.groove.swapping() {
        stem_to[DRUMS] = 0.0; // a DRUM SWAP loop replaces the drums (the ramp below makes it click-free)
    }
    let stem_step = 1.0 / (STEM_RAMP_S * dev_rate as f32);
    let lp: Loop = guard.as_ref().filter(|_| !scratching).and_then(|p| {
        unpack(sh.lp.load(Relaxed))
            .map(|(s, e)| (s * p.rate as f64 / 1000.0, e * p.rate as f64 / 1000.0))
    });
    let reverse = t.reverse() && !scratching;
    if reverse && playing {
        start = start.min(frames - 1.0); // a seek to the very end plays backwards from the last frame
    }
    let mut seeked = seek.is_some();
    if let Some(shadow) = t.begin(start) {
        start = shadow; // slip return
        sh.pos.store(start.to_bits(), Relaxed);
        v.stretch.reset();
        v.part_stretch.reset();
        seeked = true;
    }
    // Forward play: up to LOOP_LAG past a new loop's end is the heard-vs-written lag of a loop placed from the heard playhead.
    let lag = if reverse { 0.0 } else { LOOP_LAG_S * src_rate };
    if let Some(p) = shrunk(v.lp_was, lp, start, lag) {
        start = p;
        sh.pos.store(start.to_bits(), Relaxed);
        v.stretch.reset();
        v.part_stretch.reset();
        seeked = true;
    }
    if playing && !scratching {
        v.lp_was = lp;
    }
    if scratching && (!v.scr.on || seeked) {
        v.scr.start(
            start,
            transport,
            dev_rate,
            load_f32(&sh.vinyl_touch),
            step(src_rate, rate, dev_rate),
        );
    }
    v.scr.on = scratching;
    let mut pos = start;
    let key_lock = sh.key_lock.load(Relaxed);
    if t.take_unbrake() {
        v.brake.clear(); // PLAY / a hot cue during the PAUSE brake: the deck plays on
    }
    let request = t.take_brake();
    match request {
        _ if !playing || scratching => v.brake.clear(),
        Some(secs) => v.brake.start(secs, dev_rate),
        None => {}
    }
    let braking = v.brake.active();
    // Vinyl speed adjust: a pending speed-up (the platter let go of a playing deck, or PLAY from stopped) starts the ramp.
    let spin_req = if playing && !scratching {
        Some(f32::from_bits(sh.spin.swap(0, Relaxed))).filter(|&s| s > 0.0)
    } else {
        None
    };
    match spin_req {
        _ if !playing || scratching || braking => v.spin.clear(),
        Some(secs) => v.spin.start(secs, dev_rate),
        None => {}
    }
    let spinning = v.spin.active();
    // DRUM SWAP keeps time on the beat grid for the buffer (the lock is held until the end: the UI replaces the grid rarely).
    let beats = sh.beats.try_lock();
    let grid = beats
        .as_deref()
        .and_then(|b| b.as_deref())
        .map(|b| (b, src_rate));
    let link = if !playing {
        Link::Stopped
    } else if scratching || reverse || braking || spinning {
        Link::Lost
    } else {
        Link::Held
    };
    v.groove.begin(&sh.groove, link, n, dev_rate);
    let shift = t.shift();
    let stretched = playing && !braking && !spinning && !scratching && (key_lock || shift != 0.0);
    if playing && stretched != v.stretched {
        v.fade = FADE;
    }
    v.stretched = stretched;
    if !stretched {
        v.stretch.reset();
        v.part_stretch.reset();
    }
    let to_filter = v.cfx.begin(&sh.cfx); // the knob while the COLOR FX type is FILTER, else 0
    v.fader.set(sh.fader.settings());
    let fader = !raw && !v.fader.idle(); // a plugin curve that is off and rung out leaves the signal alone
    v.fx.set(
        dev_rate as f64,
        sh.eq.each_ref().map(load_f32),
        to_filter,
        n,
    );
    let dir = if reverse { -1.0 } else { 1.0 };
    // Grain read step of the stretcher (pitch) and clock step per output frame (tempo), source frames.
    let kp = dir * step(src_rate, 1.0, dev_rate) * transport::pitch(shift, key_lock, rate);
    let kr = dir * step(src_rate, rate, dev_rate);
    let nominal = step(src_rate, 1.0, dev_rate);
    // Scratch: track motion since the last buffer, spread evenly over this one (source frames per output frame).
    let moved = if scratching && playing {
        sh.scratch_ns.swap(0, Relaxed) as f64 * 1e-9 * src_rate / n as f64
    } else {
        0.0
    };
    let (mut peak, mut ended, mut fsum, mut played) = (0f32, false, 0f64, 0usize);
    let cut_step = 1.0 / (CUT_S * dev_rate as f32);
    let cut_target = if sh.cut.load(Relaxed) { 0.0 } else { 1.0 };
    let split = stems.is_some() && v.mask != 0 && !v.part_fx.idle();
    if playing && split != v.split {
        v.fade = FADE;
        v.stretch.reset();
        v.part_stretch.reset();
    }
    v.split = split;
    let mut vel = kr.abs(); // source frames the last output frame moved: the width of the interpolator's low-pass
    for i in 0..n {
        if stems.is_some() {
            for k in 0..COUNT {
                v.sg[k] += (stem_to[k] - v.sg[k]).clamp(-stem_step, stem_step);
            }
        } else {
            v.sg = [1.0; COUNT];
        }
        let mixed = stems.filter(|_| v.sg != [1.0; COUNT]);
        let (sg, mask) = (v.sg, v.mask);
        let lr = guard.as_ref().filter(|_| !ended).and_then(|p| {
            let p: &Track16 = p;
            let st = stretched.then_some(&mut v.stretch);
            match (stems, mixed) {
                (Some(s), _) if split => {
                    let source = Rest {
                        mixed: Mixed {
                            pcm: p,
                            stems: s,
                            gain: sg,
                        },
                        part: Part {
                            stems: s,
                            gain: sg,
                            mask,
                        },
                    };
                    let (rest, x) = if let Some(st) = st {
                        let rest = read(&source, Some(st), pos, frames, (kp, vel), lp)?;
                        let x = read(
                            &source.part,
                            Some(&mut v.part_stretch),
                            pos,
                            frames,
                            (kp, vel),
                            lp,
                        )
                        .unwrap_or([0.0; 2]);
                        (rest, x)
                    } else {
                        source.frame_with_part(pos)?
                    };
                    let y = v.part_fx.process(x);
                    Some([rest[0] + y[0], rest[1] + y[1]])
                }
                (_, Some(s)) => read(
                    &Mixed {
                        pcm: p,
                        stems: s,
                        gain: sg,
                    },
                    st,
                    pos,
                    frames,
                    (kp, vel),
                    lp,
                ),
                _ => read(p, st, pos, frames, (kp, vel), lp),
            }
        });
        let Some([mut l, mut r]) = lr else {
            if guard.is_some() {
                sh.playing.store(false, Relaxed); // end of track: stop
            }
            let tail = if v.part_fx.idle() {
                [0.0; 2]
            } else {
                v.part_fx.process([0.0; 2])
            };
            let (l, r) = if raw {
                (tail[0], tail[1])
            } else {
                v.cfx.process(tail[0], tail[1]) // echo / reverb tails ring out after a pause or the end of the track
            };
            let (g, gc) = (gain_at(g0, g1, i, n), gain_at(c0, c1, i, n));
            let [fl, fr] = if fader {
                v.fader.process([l, r])
            } else {
                [l, r]
            };
            peak = peak.max((fl * g).abs()).max((fr * g).abs());
            put(i, [fl * g, fr * g], [l * gc, r * gc]);
            continue;
        };
        let gr = v.groove.tick(grid, pos, kr, i);
        (l, r) = (l + gr[0], r + gr[1]);
        let sp = v.spin.tick();
        let f = v.brake.tick() * sp;
        let gs: f32;
        if scratching {
            let from = pos;
            (pos, gs) = v.scr.step(moved, (frames - 1.0).max(0.0), nominal, kr);
            vel = (pos - from).abs();
        } else {
            pos = advance(pos, kr * f as f64, lp);
            vel = (kr * f as f64).abs();
            gs = (sp * rate.abs() as f32 / SPIN_GATE).min(1.0); // a speed near 0 (a stopped tempo fader too) is silent, not one held sample
        }
        played += 1;
        if braking || spinning {
            fsum += f as f64;
        }
        if f == 0.0 || (reverse && pos < 0.0) {
            ended = true; // brake done / start of the track reached going backwards: stop here
            sh.playing.store(false, Relaxed);
            t.settle(); // stopped where it stands: no return to a shadow that stopped with it
            v.brake.clear();
        }
        if v.fade > 0 {
            let g = 1.0 - v.fade as f32 / FADE as f32;
            (l, r) = (l * g, r * g);
            v.fade -= 1;
        }
        v.cut += (cut_target - v.cut).clamp(-cut_step, cut_step);
        (l, r) = (l * gs * v.cut, r * gs * v.cut);
        let (l, r) = if raw {
            (l, r)
        } else {
            let (l, r) = v.fx.process(l, r);
            v.cfx.process(l, r)
        };
        let g = gain_at(g0, g1, i, n);
        let gc = gain_at(c0, c1, i, n);
        let [fl, fr] = if fader {
            v.fader.process([l, r])
        } else {
            [l, r]
        };
        let (ml, mr) = (fl * g, fr * g);
        peak = peak.max(ml.abs()).max(mr.abs());
        put(i, [ml, mr], [l * gc, r * gc]);
    }
    v.groove.end(&sh.groove);
    drop(guard);
    if played > 0 && transport {
        t.advance_shadow(played as f64 * step(src_rate, base, dev_rate), frames);
    }
    sh.level.store(peak.to_bits(), Relaxed);
    sh.pos.store(pos.max(0.0).to_bits(), Relaxed); // a seek racing in here is still pending and wins next callback
    sh.pos_epoch.store(epoch, Release);
    sh.steady.store(!(braking || spinning), Relaxed);
    let speed = if braking || spinning {
        fsum / n as f64
    } else {
        1.0
    };
    let fps = if scratching {
        (pos - start) * dev_rate as f64 / n as f64
    } else {
        dir * rate * src_rate * speed
    };
    Done {
        seeked,
        epoch,
        start,
        playing,
        fps,
        lp,
    }
}
