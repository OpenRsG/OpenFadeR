//! Audio input for recording: a cpal input stream delivering one stereo channel pair (Preferences > Audio >
//! "Input channels": the audio input channel used as the recording source).
//!
//! [`open`] returns an [`Input`] whose [`Input::tap`] is a [`Tap`] exactly like the bus's: switch `tap.on`
//! while recording and read `tap.ring` / `tap.rate()` (so `Recorder::start(input.tap(), ..)` records the input
//! instead of the master). Frames are only pushed while `tap.on` is set. The pair is numbered from 1 (pair 1 =
//! device channels 1/2); a mono device delivers its one channel on both sides. The stream runs at the device's
//! default rate; nothing resamples, the recording takes `tap.rate()`.

use super::{Tap, device};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SampleRate, SizedSample, Stream, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

/// Frames of slack for the recorder's writer thread, as the bus tap has.
pub(super) const TAP_FRAMES: usize = 1 << 17;

/// An open input pair; dropping it closes the stream.
pub struct Input {
    tap: Arc<Tap>,
    _stream: Stream,
}

impl Input {
    pub fn tap(&self) -> Arc<Tap> {
        self.tap.clone()
    }
}

/// Device channels of input pair `n` (1-based) on a device with `channels`: a missing second channel repeats the first.
fn indices(n: u8, channels: usize) -> [usize; 2] {
    let a = 2 * (n.max(1) as usize - 1);
    [a.min(channels - 1), (a + 1).min(channels - 1)]
}

fn build<T>(
    dev: &cpal::Device,
    cfg: &StreamConfig,
    tap: Arc<Tap>,
    pair: [usize; 2],
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = cfg.channels as usize;
    dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            if !tap.on.load(Relaxed) {
                return;
            }
            for f in data.chunks_exact(ch) {
                tap.ring
                    .push([f32::from_sample_(f[pair[0]]), f32::from_sample_(f[pair[1]])]);
            }
        },
        |e| eprintln!("audio input: {e}"),
        None,
    )
}

/// Opens input pair `pair` (1-based) of the device named `device` (empty = the system default input) and starts it.
/// Errors name the problem (no such device, too few channels, unsupported sample format).
pub fn open(device: &str, pair: u8) -> Result<Input, String> {
    open_with(device, pair, None)
}

/// [`open`] delivering into an existing `tap` (its rate is set to the input's); the caller switches `tap.on`.
pub fn open_into(device: &str, pair: u8, tap: Arc<Tap>) -> Result<Input, String> {
    open_with(device, pair, Some(tap))
}

fn open_with(device: &str, pair: u8, tap: Option<Arc<Tap>>) -> Result<Input, String> {
    let dev = if device.is_empty() {
        cpal::default_host()
            .default_input_device()
            .ok_or("no audio input device")?
    } else {
        cpal::default_host()
            .input_devices()
            .map_err(|e| format!("cannot list input devices: {e}"))?
            .find(|d| d.name().is_ok_and(|n| n == device))
            .ok_or_else(|| format!("no input device {device:?}"))?
    };
    let name = dev.name().unwrap_or_default();
    let rate = dev
        .default_input_config()
        .map_err(|e| format!("{name}: {e}"))?
        .sample_rate();
    let need = 2 * pair.max(1) as usize;
    let have = device::input_caps(&name).map_or(0, |c| c.channels);
    // The configuration with the fewest channels that has the pair, at the default rate, f32 preferred.
    let sup = dev
        .supported_input_configs()
        .map_err(|e| format!("{name}: {e}"))?
        .filter(|c| {
            (c.channels() as usize >= need || (need == 2 && c.channels() == 1))
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
        })
        .filter(|c| {
            matches!(
                c.sample_format(),
                SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
            )
        })
        .min_by_key(|c| (c.channels(), c.sample_format() != SampleFormat::F32))
        .map(|c| c.with_sample_rate(SampleRate(rate.0)))
        .ok_or_else(|| {
            format!(
                "{name}: no input with {need} channels at {} Hz (it has {have})",
                rate.0
            )
        })?;
    let channels = sup.channels() as usize;
    let tap = match tap {
        Some(t) => {
            t.set_rate(rate.0);
            t
        }
        None => Arc::new(Tap::new(rate.0, TAP_FRAMES)),
    };
    let (cfg, idx) = (sup.config(), indices(pair, channels));
    let stream = match sup.sample_format() {
        SampleFormat::F32 => build::<f32>(&dev, &cfg, tap.clone(), idx),
        SampleFormat::I16 => build::<i16>(&dev, &cfg, tap.clone(), idx),
        _ => build::<u16>(&dev, &cfg, tap.clone(), idx),
    }
    .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok(Input {
        tap,
        _stream: stream,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_numbers_pick_adjacent_channels_and_a_mono_device_repeats_its_channel() {
        assert_eq!(indices(1, 2), [0, 1]);
        assert_eq!(indices(2, 6), [2, 3]);
        assert_eq!(indices(3, 6), [4, 5]);
        assert_eq!(indices(1, 1), [0, 0]);
        assert_eq!(indices(0, 4), [0, 1], "0 reads as pair 1");
    }
}
