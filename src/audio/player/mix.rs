//! The audio callback: renders the channels, adds the side sources and mixes into the device buffer.
//!
//! Each channel is rendered ([`render`]: master path = fader/crossfade gain, cue path = trim-only gain), its clock
//! anchor recorded, the metronome clicks added, then the paths are mixed by the Preferences' mixer mode ([`Plan`],
//! decided by [`super::output`]):
//!
//! * **Internal**: sums both paths and hands the sums to [`bus::out`] ([`mix_bus`]), so the master peak is a true
//!   post-sum peak. Each frame goes to the device's channel pairs ([`route::place`]: master, headphone feed, booth),
//!   to the recorder tap while REC is on, and to the second headphone device's ring when `OPENFADER_CUE_DEVICE`
//!   opened one ([`super::super::cue`]). The sampler joins the master sum, the browser preview the headphone feed.
//! * **External**: no mixing ([`mix_external`]): Decks 1 to 4, the Sampler and the Preview each go, raw, to their own pair
//!   ([`route::place_external`]); the Beat/PAD/MERGE FX, EQ, filters, faders, crossfader and master gain are out
//!   of the path. The recorder tap then carries the plain sum of the six outputs.
//!
//! **Silent path** ([`all_zero`]): once the channels, the effects and the side sources have been rendered, a buffer whose every
//! sample (master and cue paths of all channels, and the sampler / preview pair in External mode) is exactly `+0.0` skips the
//! bus sum, the `equal_power` sin/cos per frame, the channel routing and the sample conversion: the device buffer is filled
//! with the stream's zero, the recorder tap and the second headphone ring get zeros, the gains and the peak settle where the full
//! path would leave them (`applied`, `booth`, level 0). The test is on the data, not on guessed flags, so it holds only when every
//! source is stopped *and* every effect tail (COLOR FX, Beat / PAD / MERGE / SEND / LEVER FX, stem FX, sampler release, mic)
//! has rung out to exact zero; a tail still audible, or a `-0.0`, keeps the full path, whose output is bit-identical on silence.
//! The callback allocates nothing once the buffers have grown to the device's buffer size and takes no blocking lock:
//! channel state is read through atomics and `try_lock`s ([`render`]), the clock through `try_lock` too.

use super::super::bus::{self, Bus, Params};
use super::super::route::{self, EXTERNAL, Plan};
use super::super::{Ring, fx, merge, metronome, mic, preview, sampler};
use super::engine::SLOTS;
use super::{Anchor, Shared, Voice, gain_at, render};
use cpal::{FromSample, SizedSample};
use openfader_plugin::Slot;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

type Frame = ([f32; 2], [f32; 2]);

/// Audio-thread state of the stream.
pub(super) struct Mix {
    voices: [Voice; SLOTS],
    shared: [Arc<Shared>; SLOTS],
    bus: Arc<Bus>,
    /// Master and headphone gain at the end of the last buffer (ramped like the channel gains).
    applied: [f32; 2],
    /// Per channel and frame: (master path, cue path). Grows once to the largest buffer seen.
    /// ponytail: that first growth allocates on the audio thread; preallocate if drivers resize buffers mid-run.
    buf: [Vec<Frame>; SLOTS],
    /// Headphone ring of the second device, if one is open.
    cue: Option<Arc<Ring>>,
    /// Which device channels carry what.
    plan: Plan,
    /// The device frame under construction, one slot per device channel.
    frame: Vec<f32>,
    /// Booth gain at the end of the last buffer.
    booth: f32,
    /// External mode: the sampler (`.0`) and the preview (`.1`) per frame. Grows once like `buf`.
    side: Vec<Frame>,
    /// Per channel: the last clock anchor could not be recorded (lock busy), so the next one resets the history.
    clock_missed: [bool; SLOTS],
    /// Beat FX slots FX1-1 … FX2-3 ([`fx`]): each on one channel's master path, or on their sum, before the bus.
    fx: [[fx::Unit; bus::FX_SLOTS]; bus::FX_UNITS],
    /// PAD FX units, one per channel: second [`fx::Unit`]s the performance pads switch on while held, after the Beat FX.
    pad_fx: [fx::Unit; SLOTS],
    /// MERGE FX units, one per channel ([`merge`]): on the channel's master path after the PAD FX.
    merge: [merge::Unit; SLOTS],
    /// Sampler ([`sampler`]): added to the summed master (and, with CUE on, cue) path after the Beat FX.
    sampler: sampler::Sampler,
    /// Metronome clicks (pre-rendered) and one click voice per channel.
    clicks: metronome::Clicks,
    metro: [metronome::Voice; SLOTS],
    preview: preview::Voice,
    /// Microphone ([`mic`]): added to the summed master after the sampler, ducking the rest while it talks.
    mic: mic::Voice,
    /// SEND FX rack ([`Slot::Send`]): fed by the channels' send knobs, its return joins channel A's master path.
    send: fx::Unit,
    /// LEVER FX rack ([`Slot::Lever`]) on the master sum.
    lever: fx::Unit,
    /// The last buffer took the silent path ([`all_zero`]).
    silent: bool,
}

/// Sum the channels' master and cue paths per frame, route them through the bus, hand each frame's
/// [`bus::Out`] to `put(i, out)`; returns the peak of the summed master (after the master gain, before routing).
fn mix_bus(
    buf: &[Vec<Frame>],
    n: usize,
    applied: &mut [f32; 2],
    target: Params,
    mut put: impl FnMut(usize, bus::Out),
) -> f32 {
    let [g0m, g0h] = std::mem::replace(applied, [target.master, target.headphone]);
    let mut peak = 0f32;
    for i in 0..n {
        let (mut m, mut c) = ([0f32; 2], [0f32; 2]);
        for b in buf {
            for k in 0..2 {
                m[k] += b[i].0[k];
                c[k] += b[i].1[k];
            }
        }
        let p = Params {
            master: gain_at(g0m, target.master, i, n),
            headphone: gain_at(g0h, target.headphone, i, n),
            ..target
        };
        peak = peak.max(m[0].abs() * p.master).max(m[1].abs() * p.master);
        put(i, bus::out(m, c, &p));
    }
    peak
}

/// External mixer mode: per frame the six raw outputs (decks' master paths, then `side`'s sampler and preview) and their
/// plain sum go to `put(i, outs, sum)`; returns the largest sample of any output.
fn mix_external(
    buf: &[Vec<Frame>],
    side: &[Frame],
    n: usize,
    mut put: impl FnMut(usize, [[f32; 2]; EXTERNAL], [f32; 2]),
) -> f32 {
    let mut peak = 0f32;
    for i in 0..n {
        let outs: [[f32; 2]; EXTERNAL] = std::array::from_fn(|k| match k {
            k if k < SLOTS => buf[k][i].0,
            SLOTS => side[i].0,
            _ => side[i].1,
        });
        let mut sum = [0f32; 2];
        for o in &outs {
            for c in 0..2 {
                sum[c] += o[c];
                peak = peak.max(o[c].abs());
            }
        }
        put(i, outs, sum);
    }
    peak
}

/// Every frame of the first `n` of each of `bufs` (both paths) is exactly `+0.0` (bits all zero, so `-0.0` and NaN do not
/// count: the full path would carry them on). A branch-free OR over the bits, so it vectorises.
fn all_zero(bufs: &[Vec<Frame>], n: usize) -> bool {
    bufs.iter().all(|b| {
        b[..n].iter().fold(0u32, |acc, (m, c)| {
            acc | m[0].to_bits() | m[1].to_bits() | c[0].to_bits() | c[1].to_bits()
        }) == 0
    })
}

/// The finished f32 device frame into the stream's sample type.
fn emit<T: SizedSample + FromSample<f32>>(out: &mut [T], frame: &[f32]) {
    for (o, v) in out.iter_mut().zip(frame) {
        *o = T::from_sample(*v);
    }
}

/// The cpal output callback: the buffer's latency from the host timestamps, then [`Mix::fill`].
pub(super) fn callback<T: SizedSample + FromSample<f32>>(
    mix: &mut Mix,
    dev_rate: u32,
    ch: usize,
    out: &mut [T],
    info: &cpal::OutputCallbackInfo,
) {
    let at = Instant::now();
    let ts = info.timestamp();
    let latency = ts
        .playback
        .duration_since(&ts.callback)
        .unwrap_or_else(|| Duration::from_secs_f64(out.len() as f64 / ch as f64 / dev_rate as f64));
    mix.fill(out, dev_rate, ch, at, latency);
}

/// SEND FX: the effect hears `sends[k]` times each channel's master path; its return (no dry) is added to channel A.
fn send_fx(
    unit: &mut fx::Unit,
    c: &fx::Controls,
    sends: &[f32; SLOTS],
    buf: &mut [Vec<Frame>],
    n: usize,
) {
    unit.set(c.settings());
    if unit.idle() {
        return;
    }
    for i in 0..n {
        let mut feed = [0.0f32; 2];
        for (b, s) in buf.iter().zip(sends) {
            feed[0] += s * b[i].0[0];
            feed[1] += s * b[i].0[1];
        }
        let r = unit.process(feed);
        buf[0][i].0[0] += r[0];
        buf[0][i].0[1] += r[1];
    }
}

impl Mix {
    /// Renders one device buffer of `out.len() / ch` frames; `at` is the callback's wall-clock time, `latency` the delay
    /// until the buffer is audible.
    fn fill<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        dev_rate: u32,
        ch: usize,
        at: Instant,
        latency: Duration,
    ) {
        let n = out.len() / ch;
        let Mix {
            voices,
            clock_missed,
            shared,
            bus,
            applied,
            buf,
            cue,
            plan,
            frame,
            booth,
            side,
            fx,
            pad_fx,
            merge,
            sampler,
            clicks,
            metro,
            preview,
            mic,
            send,
            lever,
            silent,
        } = self;
        bus.set_frames(n as u32);
        if side.len() < n {
            side.resize(n, ([0.0; 2], [0.0; 2]));
        }
        let mut hits = [metronome::Hits::NONE; SLOTS];
        for k in 0..SLOTS {
            if buf[k].len() < n {
                buf[k].resize(n, ([0.0; 2], [0.0; 2]));
            }
            let b = &mut buf[k];
            // ponytail: one stem part unit per voice. The first Beat FX slot that is ON on a part of this channel wins
            // (else the first one ringing out); all of them win over the deck's DRUM RELEASE FX.
            voices[k].set_part_fx(
                bus.fx
                    .as_flattened()
                    .iter()
                    .filter_map(|c| c.stem_target(k))
                    .reduce(|a, b| if !a.1.on && b.1.on { b } else { a })
                    .or_else(|| shared[k].groove.release_target(k)),
            );
            let d = render(&shared[k], &mut voices[k], dev_rate, n, |i, m, c| {
                b[i] = (m, c)
            });
            hits[k] = shared[k].beat_hits(&d, dev_rate, n);
            // A UI read holding the clock costs this buffer its anchor; the next one then restarts the history.
            let Some(mut clock) = shared[k].clock.try_lock() else {
                clock_missed[k] = true;
                continue;
            };
            let reset = d.seeked || !clock.playing || std::mem::take(&mut clock_missed[k]);
            clock.push(
                Anchor {
                    at,
                    pos: d.start,
                    latency,
                    fps: d.fps,
                    lp: d.lp,
                },
                d.playing,
                reset,
            );
        }
        let external = plan.external;
        if !external {
            for (u, c) in fx.as_flattened_mut().iter_mut().zip(bus.fx.as_flattened()) {
                u.apply(c, buf, n);
            }
            for (u, c) in pad_fx.iter_mut().zip(&bus.pad_fx) {
                u.apply(c, buf, n);
            }
            for ((u, c), b) in merge.iter_mut().zip(&bus.merge).zip(buf.iter_mut()) {
                u.apply(c, &mut b[..n]);
            }
            send_fx(send, &bus.send, &bus.sends(), buf, n);
            lever.apply(&bus.lever, buf, n);
        }
        let (tone, volume) = bus.metronome();
        for k in 0..SLOTS {
            metro[k].mix(clicks, tone, volume, &hits[k], &mut buf[k][..n]);
        }
        let rec = bus.tap.on.load(Relaxed);
        if external {
            side[..n].fill(([0.0; 2], [0.0; 2]));
            sampler.add(&bus.sampler, &mut side[..n]);
            for s in &mut side[..n] {
                s.1 = [0.0; 2]; // the sampler's cue copy has no place here: `.1` is the preview
            }
            preview.render(&bus.preview, dev_rate, n, |i, f| side[i].1 = f);
            *silent = all_zero(buf, n) && all_zero(std::slice::from_ref(side), n);
            if *silent {
                out.fill(T::from_sample(0.0f32));
                if rec {
                    for _ in 0..n {
                        bus.tap.ring.push([0.0; 2]); // the plain sum of six zeros
                    }
                }
                bus.set_level(0.0);
                return;
            }
            let peak = mix_external(buf, side, n, |i, outs, sum| {
                route::place_external(frame, plan, &outs);
                emit(&mut out[i * ch..(i + 1) * ch], frame);
                if rec {
                    bus.tap.ring.push(sum);
                }
            });
            bus.set_level(peak);
            return;
        }
        sampler.add(&bus.sampler, &mut buf[0][..n]); // after the Beat FX: the sampler is on the master sum, not on channel A
        mic.mix(&bus.mic, &bus.mic_fx, dev_rate, buf, n); // talkover ducks the whole sum, then the voice joins it
        preview.render(&bus.preview, dev_rate, n, |i, f| {
            for c in 0..2 {
                buf[0][i].1[c] += f[c]; // the preview rides the headphone feed
            }
        });
        let mut target = bus.params();
        target.split &= cue.is_none() && plan.phones.is_none(); // a second device or its own channels replace SPLIT
        let (b0, b1) = (std::mem::replace(booth, bus.booth()), bus.booth());
        *silent = all_zero(buf, n);
        if *silent {
            *applied = [target.master, target.headphone]; // where `mix_bus` leaves the ramp
            out.fill(T::from_sample(0.0f32));
            for _ in 0..n {
                if let Some(c) = cue {
                    c.push([0.0; 2]);
                }
                if rec {
                    bus.tap.ring.push([0.0; 2]);
                }
            }
            bus.set_level(0.0);
            return;
        }
        let peak = mix_bus(buf, n, applied, target, |i, o| {
            route::place(
                frame,
                plan,
                o.device,
                o.master,
                o.phones,
                gain_at(b0, b1, i, n),
            );
            emit(&mut out[i * ch..(i + 1) * ch], frame);
            if let Some(c) = cue {
                c.push(o.phones);
            }
            if rec {
                bus.tap.ring.push(o.master);
            }
        });
        bus.set_level(peak);
    }
    /// The audio thread's state for a stream of `ch` channels at `rate`, on `plan`, with the second headphone device's ring `cue`.
    pub(super) fn new(
        rate: u32,
        ch: usize,
        plan: Plan,
        cue: Option<Arc<Ring>>,
        shared: &[Arc<Shared>; SLOTS],
        bus: &Arc<Bus>,
    ) -> Mix {
        let target = bus.params();
        Mix {
            clock_missed: [false; SLOTS],
            voices: [(); SLOTS].map(|_| Voice::new(rate)),
            shared: shared.clone(),
            bus: bus.clone(),
            applied: [target.master, target.headphone], // continue from the knobs, no ramp from the defaults after a device switch
            buf: [(); SLOTS].map(|_| Vec::with_capacity(4096)),
            cue,
            plan,
            frame: vec![0.0; ch],
            booth: bus.booth(),
            side: Vec::with_capacity(4096),
            fx: std::array::from_fn(|_| std::array::from_fn(|_| fx::Unit::new(rate))),
            pad_fx: std::array::from_fn(|_| fx::Unit::new(rate)),
            merge: std::array::from_fn(|_| merge::Unit::new(rate)),
            sampler: sampler::Sampler::new(rate),
            clicks: metronome::Clicks::new(rate),
            metro: [(); SLOTS].map(|_| metronome::Voice::default()),
            preview: preview::Voice::new(),
            mic: mic::Voice::new(rate),
            send: fx::Unit::rack(Slot::Send, rate),
            lever: fx::Unit::rack(Slot::Lever, rate),
            silent: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::route::Settings;
    use super::*;

    /// Four channels of `n` frames: channel 0 = 0.5, channel 1 = 0.25, the others silent.
    fn four(n: usize) -> [Vec<Frame>; SLOTS] {
        std::array::from_fn(|k| vec![([[0.5, 0.25, 0.0, 0.0][k]; 2], [0.0; 2]); n])
    }

    // GAIN (doubles the signal, from the plugin tests' fake library) stands in for a SEND / LEVER plugin.
    #[test]
    fn the_send_effect_hears_only_raised_channels_and_its_return_joins_channel_a() {
        let fx_list = crate::plugin::effect::fake_effects();
        let c = fx::Controls::default();
        c.set_level(1.0);
        c.set_on(true);
        let mut u = fx::Unit::rack_with(48_000, &fx_list);
        let mut buf = four(2000);
        send_fx(&mut u, &c, &[0.0, 1.0, 0.0, 0.0], &mut buf, 2000);
        let end = buf[0][1999].0[0];
        assert!(
            (end - (0.5 + 0.5)).abs() < 1e-4,
            "return 2 x 0.25 added: {end}"
        );
        assert_eq!(buf[1][1999].0[0], 0.25, "the channel itself is untouched");
        let mut dry = four(2000);
        send_fx(&mut u, &c, &[0.0; SLOTS], &mut dry, 2000);
        assert_eq!(dry[0][1999].0[0], 0.5, "no send up, nothing returns");
    }

    #[test]
    fn a_lever_at_rest_leaves_the_master_bit_exact() {
        let fx_list = crate::plugin::effect::fake_effects();
        let c = fx::Controls::default();
        c.set_assign(fx::Assign::Master);
        c.set_level(1.0);
        let mut u = fx::Unit::rack_with(48_000, &fx_list);
        let (mut buf, want) = (four(256), four(256));
        u.apply(&c, &mut buf, 256);
        assert!(buf.iter().zip(&want).all(|(a, b)| a == b));
        c.set_on(true);
        u.apply(&c, &mut buf, 256);
        for _ in 0..8 {
            u.apply(&c, &mut buf, 256);
        }
        assert_ne!(buf[0], want[0], "pulled, the master changes");
    }

    #[test]
    fn mix_bus_sums_channels_and_reports_the_post_sum_master_peak() {
        let frame = |l: f32, r: f32| ([l, r], [l, r]);
        let buf = [vec![frame(0.25, 0.25); 4], vec![frame(0.5, -0.5); 4]];
        let mut applied = [0.5, 1.0];
        let mut out = vec![];
        let t = Params {
            master: 0.5,
            split: false,
            mix: 0.0,
            headphone: 1.0,
        };
        let peak = mix_bus(&buf, 4, &mut applied, t, |_, o| out.push(o));
        // master ramps 0.5 -> 0.5 (already there): sum (0.75, -0.25) * 0.5
        assert_eq!(out[3].device, [0.375, -0.125]);
        assert_eq!(out[3].master, [0.375, -0.125]);
        assert_eq!(peak, 0.375); // peak of the sum, not of either channel alone (0.25, 0.5 each)
        assert_eq!(applied, [0.5, 1.0]);
        // headphone feed at mix 0 = the summed cue (0.75, -0.25), stereo
        assert_eq!(out[3].phones, [0.75, -0.25]);
        // split: left = mono cue (0.25), right = mono master ((0.75 - 0.25) / 2 * 0.5); the recorder still gets the stereo master
        out.clear();
        mix_bus(
            &buf,
            4,
            &mut applied,
            Params { split: true, ..t },
            |_, o| out.push(o),
        );
        assert_eq!(out[3].device, [0.25, 0.125]);
        assert_eq!(out[3].master, [0.375, -0.125]);
        // the master gain ramps across the buffer from the last applied value
        out.clear();
        mix_bus(
            &buf,
            4,
            &mut applied,
            Params {
                split: false,
                master: 1.0,
                ..t
            },
            |_, o| out.push(o),
        );
        assert!(out[0].device[0] < out[3].device[0] && out[3].device[0] == 0.75);
    }

    /// Two summed channels through the bus into an interleaved `ch`-channel buffer, like the callback does.
    fn render_internal(ch: usize, plan: &Plan) -> Vec<f32> {
        let buf = [
            vec![([0.25, 0.5], [0.1, 0.2]); 3],
            vec![([0.25, 0.0], [0.0, 0.2]); 3],
        ]; // master (0.5, 0.5), cue (0.1, 0.4)
        let mut out = vec![9.0f32; 3 * ch]; // stale data: every channel must be written
        let t = Params {
            master: 1.0,
            split: false,
            mix: 0.0,
            headphone: 0.5,
        };
        let mut frame = vec![0.0f32; ch];
        mix_bus(&buf, 3, &mut [1.0, 0.5], t, |i, o| {
            route::place(&mut frame, plan, o.device, o.master, o.phones, 0.5);
            emit(&mut out[i * ch..(i + 1) * ch], &frame);
        });
        out
    }

    fn plan(master: [usize; 2], phones: Option<[usize; 2]>, booth: Option<[usize; 2]>) -> Plan {
        Plan {
            master,
            phones,
            booth,
            ..Plan::stereo()
        }
    }

    #[test]
    fn four_channel_device_gets_master_on_1_2_headphones_on_3_4() {
        let out = render_internal(4, &plan([0, 1], Some([2, 3]), None));
        assert_eq!(out[..4], [0.5, 0.5, 0.05, 0.2]); // master L/R, then the headphone feed (cue 0.1/0.4 at mix 0, level 0.5)
        assert_eq!(out[8..], [0.5, 0.5, 0.05, 0.2]);
    }

    #[test]
    fn pair_choices_move_the_feeds_and_leave_the_rest_silent() {
        let out = render_internal(6, &plan([0, 1], Some([4, 5]), None));
        assert_eq!(out[..6], [0.5, 0.5, 0.0, 0.0, 0.05, 0.2]);
        let out = render_internal(6, &plan([0, 1], Some([3, 2]), None)); // swapped pair: L on channel 4, R on channel 3
        assert_eq!(out[..6], [0.5, 0.5, 0.2, 0.05, 0.0, 0.0]);
        let out = render_internal(6, &plan([2, 3], None, Some([4, 5]))); // master on 3/4, booth = master x 0.5 on 5/6
        assert_eq!(out[..6], [0.0, 0.0, 0.5, 0.5, 0.25, 0.25]);
    }

    #[test]
    fn without_a_pair_the_device_stays_stereo_and_extra_channels_are_silent() {
        assert_eq!(
            render_internal(4, &plan([0, 1], None, None))[..4],
            [0.5, 0.5, 0.0, 0.0]
        );
    }

    #[test]
    fn external_mode_sends_each_source_raw_to_its_pair_and_sums_them_for_the_recorder() {
        let deck = |l: f32| vec![([l, -l], [9.0, 9.0]); 2]; // the cue path never matters here
        let buf = [deck(0.1), deck(0.2), deck(0.3), deck(0.4)];
        let side = vec![([0.5, 0.5], [0.6, 0.7]); 2]; // sampler, preview
        let s = Settings {
            mixer: route::Mixer::External,
            ..Settings::default()
        };
        let (plan, _) = Plan::new(&s, None, 12).unwrap();
        let (mut frame, mut seen) = (vec![0.0f32; 12], vec![]);
        let peak = mix_external(&buf, &side, 2, |i, outs, sum| {
            route::place_external(&mut frame, &plan, &outs);
            seen.push((i, frame.clone(), sum));
        });
        assert_eq!(
            seen[1].1,
            [
                0.1, -0.1, 0.2, -0.2, 0.3, -0.3, 0.4, -0.4, 0.5, 0.5, 0.6, 0.7
            ]
        );
        assert_eq!(
            seen[0].2,
            [
                0.1 + 0.2 + 0.3 + 0.4 + 0.5 + 0.6,
                -0.1 - 0.2 - 0.3 - 0.4 + 0.5 + 0.7
            ]
        );
        assert_eq!(peak, 0.7);
    }

    /// Counts the allocations of the thread that reads it, so a test can prove a stretch of code makes none.
    struct Counting;
    thread_local!(static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
    // SAFETY: forwards every call unchanged to `System`; the counter is a plain thread-local `Cell`.
    unsafe impl std::alloc::GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
            ALLOCS.with(|c| c.set(c.get() + 1));
            unsafe { std::alloc::System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
            ALLOCS.with(|c| c.set(c.get() + 1));
            unsafe { std::alloc::System.realloc(p, l, n) }
        }
    }
    #[global_allocator]
    static COUNTING: Counting = Counting;

    #[test]
    fn the_callback_allocates_nothing_once_the_buffers_have_grown() {
        let shared = [(); SLOTS].map(|_| Arc::new(Shared::new()));
        let bus = Arc::new(Bus::new(44100, bus::Headphones::Split));
        for (k, sh) in shared.iter().enumerate().take(2) {
            let samples = (0..44100 * 3)
                .flat_map(|i| {
                    let v = (i as f32 * 0.06).sin() * 0.5;
                    [v, v]
                })
                .collect();
            *sh.pcm.lock() = Arc::new(
                super::super::super::Pcm {
                    rate: 44100,
                    samples,
                }
                .into(),
            );
            sh.playing.store(true, Relaxed);
            sh.key_lock.store(true, Relaxed); // the WSOLA path
            sh.rate.store((1.0 + 0.05 * k as f64).to_bits(), Relaxed);
            sh.t.set_shift(2.0);
            sh.cfx.set_knob(-0.4);
            sh.set_loop(Some((500, 900)));
        }
        let mut mix = Mix::new(44100, 2, Plan::stereo(), None, &shared, &bus);
        let mut out = vec![0f32; 2 * 512];
        let (at, latency) = (Instant::now(), Duration::from_millis(10));
        for _ in 0..8 {
            mix.fill(&mut out, 44100, 2, at, latency); // grows `buf` and `side`
        }
        let probe = ALLOCS.with(|c| c.get());
        drop(std::hint::black_box(vec![0u8; 8]));
        assert!(
            ALLOCS.with(|c| c.get()) > probe,
            "the counting allocator is live"
        );
        let before = ALLOCS.with(|c| c.get());
        for _ in 0..200 {
            mix.fill(&mut out, 44100, 2, at, latency);
        }
        assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
        assert!(out.iter().any(|&s| s != 0.0), "the decks are audible");
    }

    #[test]
    fn the_bundled_json_effects_allocate_nothing_per_frame() {
        if !crate::plugin::effect::kit::have_bundled() {
            return;
        }
        let all = crate::plugin::effect::kit::bundled_all();
        assert!(all.len() >= 25, "{} bundled effects", all.len());
        let mut stages: Vec<_> = all.iter().map(|e| e.stage(48_000)).collect();
        let c = fx::Ctx {
            rate: 48_000.0,
            period: 24_000.0,
            beats: 1.0,
            level: 1.0,
            g: 1.0,
            knob: 0.6,
        };
        for s in &mut stages {
            s.engage();
            for i in 0..100 {
                s.tick([(i as f32 * 0.05).sin() * 0.5; 2], &c); // the first frames decide capture lengths
            }
        }
        let before = ALLOCS.with(|c| c.get());
        for s in &mut stages {
            for i in 0..4000 {
                s.tick([(i as f32 * 0.05).sin() * 0.5; 2], &c);
            }
            s.engage();
            s.tick([0.1; 2], &c);
            s.reset();
        }
        assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
    }

    /// A `Mix` on stereo over fresh decks and bus.
    fn silent_rig() -> ([Arc<Shared>; SLOTS], Arc<Bus>, Mix) {
        let shared = [(); SLOTS].map(|_| Arc::new(Shared::new()));
        let bus = Arc::new(Bus::new(44100, bus::Headphones::Split));
        let mix = Mix::new(44100, 2, Plan::stereo(), None, &shared, &bus);
        (shared, bus, mix)
    }

    #[test]
    fn the_silent_path_writes_exactly_what_the_full_path_makes_of_silence() {
        let (_shared, bus, mut mix) = silent_rig();
        bus.tap.on.store(true, Relaxed);
        bus.set_level(0.7); // the silent buffer must reset the meter
        let mut out = vec![9.0f32; 2 * 512]; // stale data: the whole buffer must be written
        mix.fill(
            &mut out,
            44100,
            2,
            Instant::now(),
            Duration::from_millis(10),
        );
        assert!(
            mix.silent,
            "nothing plays and no effect rings: the silent path"
        );
        assert_eq!(bus.master_level(), 0.0);
        // The full path on the same silence: mix_bus -> place -> emit, as `fill` runs them.
        let buf: [Vec<Frame>; SLOTS] = std::array::from_fn(|_| vec![([0.0; 2], [0.0; 2]); 512]);
        let target = bus.params();
        let mut want = vec![9.0f32; 2 * 512];
        let mut frame = vec![0.0f32; 2];
        let mut applied = [target.master, target.headphone];
        let peak = mix_bus(&buf, 512, &mut applied, target, |i, o| {
            route::place(
                &mut frame,
                &Plan::stereo(),
                o.device,
                o.master,
                o.phones,
                0.0,
            );
            emit(&mut want[i * 2..(i + 1) * 2], &frame);
        });
        assert_eq!(peak, 0.0);
        assert!(
            out.iter()
                .zip(&want)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert_eq!(mix.applied, applied);
        let mut tapped = 0;
        while let Some(f) = bus.tap.ring.pop() {
            assert_eq!(f.map(f32::to_bits), [0, 0], "the recorder hears zeros too");
            tapped += 1;
        }
        assert!(tapped > 0);
    }

    #[test]
    fn the_silent_path_engages_only_after_every_tail_has_rung_out() {
        let (shared, _bus, mut mix) = silent_rig();
        let sh = &shared[0];
        let samples = (0..44100 * 3)
            .flat_map(|i| {
                let v = (i as f32 * 0.06).sin() * 0.5;
                [v, v]
            })
            .collect();
        *sh.pcm.lock() = Arc::new(
            super::super::super::Pcm {
                rate: 44100,
                samples,
            }
            .into(),
        );
        sh.cfx.set_kind(super::super::super::cfx::Kind::DubEcho);
        sh.cfx.set_knob(0.6);
        sh.playing.store(true, Relaxed);
        let mut out = vec![0f32; 2 * 512];
        let (at, latency) = (Instant::now(), Duration::from_millis(10));
        for _ in 0..100 {
            mix.fill(&mut out, 44100, 2, at, latency);
            assert!(!mix.silent, "a playing deck is never silent");
        }
        sh.playing.store(false, Relaxed); // paused, knob back to the centre: only the echo tail is left
        sh.cfx.set_knob(0.0);
        // (silent, any sample non-zero) per buffer: 35 s, far beyond the echo's hold and its decay to -120 dBFS.
        let seen: Vec<(bool, bool)> = (0..3000)
            .map(|_| {
                mix.fill(&mut out, 44100, 2, at, latency);
                (mix.silent, out.iter().any(|&s| s != 0.0))
            })
            .collect();
        let last_audible = seen.iter().rposition(|&(_, audible)| audible);
        let first_silent = seen.iter().position(|&(silent, _)| silent);
        assert!(last_audible.is_some(), "the echo tail was heard");
        assert!(
            first_silent.is_some(),
            "the tail ends and the silent path engages"
        );
        assert!(
            last_audible < first_silent,
            "never silent while a tail is audible: {last_audible:?} vs {first_silent:?}"
        );
        assert!(
            seen[first_silent.unwrap()..]
                .iter()
                .all(|&(silent, audible)| silent && !audible),
            "and it stays engaged on silence"
        );
    }
}
