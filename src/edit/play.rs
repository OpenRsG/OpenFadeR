//! Preview of the arrangement on the default output device: its own cpal stream (the decks' shared stream
//! stays untouched), reading a rendered [`Pcm`] the callback owns, so the callback allocates nothing and
//! takes no lock. The position goes back through one atomic. Dropping [`Play`] stops it.
//! Not routed through `OPENFADER_OUTPUT_DEVICE` / the headphone bus: it plays on the system default.

use crate::audio::Pcm;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering::Relaxed}};

/// The callback's side: where in the rendered audio, how far per device frame (source rate / device rate).
struct Cursor {
    pcm: Arc<Pcm>,
    pos: f64,
    step: f64,
}

impl Cursor {
    /// Writes device frames of `ch` channels into `out`: stereo into the first two, mono as the mean, the
    /// rest silent. Silence once the audio ends; returns whether it has.
    fn fill<T: Sample + FromSample<f32>>(&mut self, out: &mut [T], ch: usize) -> bool {
        let mut ended = false;
        for frame in out.chunks_mut(ch) {
            let lr = self.pcm.frame_at_step(self.pos, self.step).unwrap_or_else(|| {
                ended = true;
                [0.0; 2]
            });
            match frame {
                [m] => *m = T::from_sample((lr[0] + lr[1]) / 2.0),
                [l, r, rest @ ..] => {
                    *l = T::from_sample(lr[0]);
                    *r = T::from_sample(lr[1]);
                    rest.fill(T::EQUILIBRIUM);
                }
                [] => {}
            }
            self.pos += self.step;
        }
        ended
    }
}

/// A running preview.
pub struct Play {
    _stream: Stream,
    pos: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    dev: &cpal::Device, cfg: &cpal::StreamConfig, mut c: Cursor, pos: Arc<AtomicU64>, ended: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError> {
    let ch = cfg.channels as usize;
    dev.build_output_stream(
        cfg,
        move |o: &mut [T], _| {
            if c.fill(o, ch) {
                ended.store(true, Relaxed);
            }
            pos.store(c.pos.to_bits(), Relaxed);
        },
        |e| eprintln!("edit audio: {e}"),
        None,
    )
}

impl Play {
    /// Starts playing `pcm` from frame `from`.
    pub fn start(pcm: Arc<Pcm>, from: usize) -> Result<Play, String> {
        let dev = cpal::default_host().default_output_device().ok_or("no audio output device")?;
        let sup = dev.default_output_config().map_err(|e| e.to_string())?;
        let cfg = sup.config();
        let c = Cursor { step: pcm.rate as f64 / cfg.sample_rate.0 as f64, pos: from as f64, pcm };
        let (pos, ended) = (Arc::new(AtomicU64::new(c.pos.to_bits())), Arc::new(AtomicBool::new(false)));
        let (p, e) = (pos.clone(), ended.clone());
        let stream = match sup.sample_format() {
            SampleFormat::F32 => build::<f32>(&dev, &cfg, c, p, e),
            SampleFormat::I16 => build::<i16>(&dev, &cfg, c, p, e),
            SampleFormat::U16 => build::<u16>(&dev, &cfg, c, p, e),
            f => return Err(format!("unsupported sample format {f:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Play { _stream: stream, pos, ended })
    }

    /// Frame of the rendered audio being played (a device buffer ahead of what is heard).
    pub fn pos(&self) -> usize {
        f64::from_bits(self.pos.load(Relaxed)) as usize
    }

    /// The audio has played to its end.
    pub fn ended(&self) -> bool {
        self.ended.load(Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(frames: usize, step: f64) -> Cursor {
        let samples = (0..frames).flat_map(|i| [i as f32, -(i as f32)]).collect();
        Cursor { pcm: Arc::new(Pcm { rate: 1000, samples }), pos: 0.0, step }
    }

    #[test]
    fn plays_stereo_then_silence_and_reports_the_end() {
        let mut c = cursor(3, 1.0);
        let mut out = [9.0f32; 10];
        assert!(c.fill(&mut out, 2));
        assert_eq!(out, [0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 0.0, 0.0, 0.0, 0.0]);
        let mut c = cursor(3, 1.0);
        assert!(!c.fill(&mut [0.0f32; 4], 2) && c.pos == 2.0);
    }

    #[test]
    fn maps_channels_and_resamples() {
        // interior of a ramp (the interpolator needs ~16 frames on each side): the half position is the midpoint
        let mut c = cursor(64, 0.5);
        c.pos = 30.0;
        let mut out = [9.0f32; 6];
        c.fill(&mut out, 3); // 3 channels: L, R, silent
        let want = [30.0, -30.0, 0.0, 30.5, -30.5, 0.0];
        assert!(out.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3), "{out:?}");
        let mut c = cursor(4, 1.0);
        let mut mono = [9.0f32; 2];
        c.fill(&mut mono, 1);
        assert_eq!(mono, [0.0, 0.0]);
        let mut c = Cursor { pcm: Arc::new(Pcm { rate: 1000, samples: vec![1.0, 0.5, 1.0, 0.5] }), pos: 0.0, step: 1.0 };
        c.fill(&mut mono, 1);
        assert_eq!(mono, [0.75, 0.75]);
    }
}
