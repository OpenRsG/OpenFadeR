//! Browser preview (audition) player: one track played to the headphones, separate from the four decks.
//!
//! Preferences > Audio > Preview Volume; External mixer mode has a
//! "Preview" output of its own. Internal mode: the preview is added to the
//! headphone (cue) feed, so it is heard where CUE is heard (SPLIT, a second headphone device or the headphone pair)
//! and never reaches the master or the recording. External mode: the preview plays alone on its output pair.
//!
//! The browser owner loads a decoded track with [`Controls::load`] (reach the controls through `Bus::preview`, the
//! bus through `audio::bus()`), then `play` / `stop` / `seek_ms`. The level is the Preferences value, set by
//! `Bus::apply`. The audio thread only `try_lock`s the track (a concurrent `load` costs one silent buffer) and
//! never allocates; the replaced track is dropped by the UI thread.

use super::Track16;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

const NO_SEEK: u64 = u64::MAX;

/// What the UI sets and the callback reads.
pub struct Controls {
    pcm: Mutex<Option<Arc<Track16>>>,
    rate: AtomicU32,
    frames: AtomicU64,
    /// f64 bits, source frame of the next frame to play (written by the callback).
    pos: AtomicU64,
    /// f64 bits or [`NO_SEEK`], consumed by the callback.
    seek: AtomicU64,
    playing: AtomicBool,
    gain: AtomicU32,
}

impl Default for Controls {
    fn default() -> Controls {
        Controls {
            pcm: Mutex::new(None),
            rate: AtomicU32::new(0),
            frames: AtomicU64::new(0),
            pos: AtomicU64::new(0),
            seek: AtomicU64::new(NO_SEEK),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1f32.to_bits()),
        }
    }
}

impl Controls {
    /// Replaces the preview track (stopped, at the start).
    pub fn load(&self, pcm: Arc<Track16>) {
        self.playing.store(false, Relaxed);
        self.rate.store(pcm.rate, Relaxed);
        self.frames.store(pcm.frames() as u64, Relaxed);
        self.seek.store(0f64.to_bits(), Relaxed);
        let old = self.pcm.lock().replace(pcm);
        drop(old); // freed here, never on the audio thread
    }

    /// Plays from the current position (from the start when the track had ended).
    pub fn play(&self) {
        if self.frames.load(Relaxed) == 0 {
            return;
        }
        if self.at() >= self.frames.load(Relaxed) as f64 {
            self.seek.store(0f64.to_bits(), Relaxed);
        }
        self.playing.store(true, Relaxed);
    }

    pub fn stop(&self) {
        self.playing.store(false, Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Relaxed)
    }

    /// Moves the playhead to `ms` (clamped to the track).
    pub fn seek_ms(&self, ms: u32) {
        let f = (ms as f64 * self.rate.load(Relaxed) as f64 / 1000.0)
            .min(self.frames.load(Relaxed) as f64);
        self.seek.store(f.to_bits(), Relaxed);
    }

    /// Playhead in track ms (the pending seek counts).
    pub fn position_ms(&self) -> f64 {
        match self.rate.load(Relaxed) {
            0 => 0.0,
            r => self.at() * 1000.0 / r as f64,
        }
    }

    pub fn duration_ms(&self) -> u32 {
        match self.rate.load(Relaxed) {
            0 => 0,
            r => (self.frames.load(Relaxed) * 1000 / r as u64) as u32,
        }
    }

    /// Linear level (the Preferences "Preview Volume"); ramped across the next buffer.
    pub(super) fn set_gain(&self, gain: f32) {
        self.gain.store(
            (if gain.is_finite() { gain.max(0.0) } else { 1.0 }).to_bits(),
            Relaxed,
        );
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Relaxed))
    }

    /// Source frame of the playhead given the `seek` word just read: a pending seek wins over `pos`.
    fn head(&self, seek: u64) -> f64 {
        f64::from_bits(if seek != NO_SEEK {
            seek
        } else {
            self.pos.load(Relaxed)
        })
    }

    fn at(&self) -> f64 {
        self.head(self.seek.load(Relaxed))
    }
}

/// The callback's side: the gain as heard at the end of the last buffer.
pub struct Voice {
    applied: f32,
}

impl Voice {
    pub fn new() -> Voice {
        Voice { applied: 1.0 }
    }

    /// Hands up to `n` frames of the playing track to `put(i, frame)` (linear resample to `dev_rate`, gain ramped
    /// from the last buffer's value). Calls `put` for no frame while stopped; stops at the end of the track.
    pub fn render(
        &mut self,
        c: &Controls,
        dev_rate: u32,
        n: usize,
        mut put: impl FnMut(usize, [f32; 2]),
    ) {
        let g1 = c.gain();
        let g0 = std::mem::replace(&mut self.applied, g1);
        if !c.playing.load(Relaxed) {
            return;
        }
        let Some(guard) = c.pcm.try_lock() else {
            return;
        };
        let Some(pcm) = guard.as_ref() else { return };
        let mut pos = c.head(c.seek.swap(NO_SEEK, Relaxed));
        let step = pcm.rate as f64 / dev_rate as f64;
        for i in 0..n {
            let Some(f) = pcm.frame_at(pos) else {
                c.playing.store(false, Relaxed); // end of the track
                break;
            };
            let g = g0 + (g1 - g0) * ((i + 1) as f32 / n as f32);
            put(i, [f[0] * g, f[1] * g]);
            pos += step;
        }
        c.pos.store(pos.to_bits(), Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Pcm;

    /// A ramp: sample `i / frames`, quantised like a decoded track.
    fn track(rate: u32, frames: usize) -> Arc<Track16> {
        Arc::new(Track16::from(Pcm {
            rate,
            samples: (0..frames)
                .flat_map(|i| [i as f32 / frames as f32, -(i as f32) / frames as f32])
                .collect(),
        }))
    }

    fn render(v: &mut Voice, c: &Controls, rate: u32, n: usize) -> Vec<Option<[f32; 2]>> {
        let mut out = vec![None; n];
        v.render(c, rate, n, |i, f| out[i] = Some(f));
        out
    }

    #[test]
    fn silent_until_played_then_plays_at_the_device_rate_and_stops_at_the_end() {
        let (c, mut v) = (Controls::default(), Voice::new());
        c.load(track(24_000, 1000));
        assert!(
            render(&mut v, &c, 48_000, 8).iter().all(Option::is_none),
            "loaded is not playing"
        );
        c.play();
        let out = render(&mut v, &c, 48_000, 8);
        assert_eq!(out[0], Some([0.0, 0.0]));
        assert!(
            (out[2].unwrap()[0] - 0.001).abs() < 1e-4,
            "24 kHz track on a 48 kHz device: half a source frame per output frame"
        );
        assert!((c.position_ms() - 8.0 * 0.5 * 1000.0 / 24_000.0).abs() < 1e-6);
        let rest = render(&mut v, &c, 48_000, 4000);
        assert!(
            rest[1995..].iter().all(Option::is_none) && !c.is_playing(),
            "ends after 1992 output frames"
        );
        c.play();
        assert!(
            c.is_playing() && render(&mut v, &c, 48_000, 2)[0] == Some([0.0, 0.0]),
            "play after the end restarts"
        );
    }

    #[test]
    fn seek_gain_and_ramp() {
        let (c, mut v) = (Controls::default(), Voice::new());
        c.load(track(1000, 1000));
        c.seek_ms(500);
        assert_eq!(c.position_ms(), 500.0, "a pending seek is the position");
        assert_eq!(c.duration_ms(), 1000);
        c.set_gain(0.0);
        c.play();
        let out = render(&mut v, &c, 1000, 4);
        assert_eq!(
            out[3],
            Some([0.0, 0.0]),
            "ramps from 1.0 to the new gain 0 over the buffer"
        );
        assert!(out[0].unwrap()[0] > out[1].unwrap()[0] && out[0].unwrap()[0] > 0.0);
        c.set_gain(f32::NAN);
        assert_eq!(c.gain(), 1.0);
    }
}
