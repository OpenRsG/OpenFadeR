//! Whole-file decode (symphonia): to f32 [`Pcm`] for the sampler, edit and groove, or straight to a quantised [`Track16`] with
//! the audio fingerprint for everything that keeps a track for long (decks, browser preview, bulk analysis).

use super::{Pcm, Track16, track16::quantise};
use crate::analysis::fingerprint::Window;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, channel},
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Dropping a decode job stops its worker at the next packet boundary.
pub struct DecodeJob<T = Pcm> {
    rx: Receiver<Result<T, String>>,
    cancelled: Arc<AtomicBool>,
}

impl<T> std::ops::Deref for DecodeJob<T> {
    type Target = Receiver<Result<T, String>>;

    fn deref(&self) -> &Self::Target {
        &self.rx
    }
}

impl<T> Drop for DecodeJob<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn spawn<T: Send + 'static>(
    run: impl FnOnce(&dyn Fn() -> bool) -> Result<T, String> + Send + 'static,
) -> DecodeJob<T> {
    let (tx, rx) = channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    std::thread::spawn(move || {
        let result = run(&|| stop.load(Ordering::Relaxed));
        let _ = tx.send(result);
    });
    DecodeJob { rx, cancelled }
}

pub fn decode_async(path: PathBuf) -> DecodeJob {
    spawn(move |cancelled| decode_cancellable(&path, cancelled))
}

/// [`decode_async`] for [`decode_track16_cancellable`].
pub fn decode_track16_async(path: PathBuf) -> DecodeJob<(Track16, String)> {
    spawn(move |cancelled| decode_track16_cancellable(&path, cancelled))
}

/// Probes `path` for its container (the part of decoding that is cheap).
fn open_format(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let file = crate::paths::at(std::fs::File::open(path), path)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    Ok(probed.format)
}

/// Sample rate of `path` from its header, without decoding; `None` when it cannot be read.
pub fn probe_rate(path: &Path) -> Option<u32> {
    audio_track(&*open_format(path).ok()?)?
        .codec_params
        .sample_rate
}

/// The first audio track. Not `default_track`: in an mp4 / mov that is the video track when it comes first.
fn audio_track(fmt: &dyn FormatReader) -> Option<&Track> {
    fmt.tracks()
        .iter()
        .find(|t| t.codec_params.sample_rate.is_some())
        .or_else(|| fmt.default_track())
}

/// Decode a whole file (mp3/aac-m4a/flac/wav/aiff) to stereo: mono duplicated, >2 channels keep the first two. A video
/// file whose audio symphonia cannot decode (AC-3, Opus …) goes through `ffmpeg` instead ([`crate::video::audio`]).
/// ponytail: stereo f32 PCM needs 21.2 MB/min at 44.1 kHz; [`decode_track16_cancellable`] halves that for tracks kept long.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    decode_cancellable(path, &|| false)
}

/// Checks cancellation before opening the file and between native decoder packets.
pub fn decode_cancellable(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Pcm, String> {
    with_video_fallback(path, cancelled, decode_native, |pcm| pcm)
}

/// [`decode_cancellable`] to the 16-bit [`Track16`] (quantised packet by packet: no full-length f32 buffer, except for the
/// `ffmpeg` fallback) and the track's audio fingerprint, the same string [`crate::analysis::fingerprint::of`] gives for
/// the f32 decode.
pub fn decode_track16_cancellable(
    path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Track16, String), String> {
    with_video_fallback(path, cancelled, decode_native_track16, |pcm| {
        (Track16::from(&pcm), crate::analysis::fingerprint::of(&pcm))
    })
}

/// `native` decode of `path`, else (for a video file) the `ffmpeg` decode turned into the result by `from_pcm`.
fn with_video_fallback<T>(
    path: &Path,
    cancelled: &dyn Fn() -> bool,
    native: fn(&Path, &dyn Fn() -> bool) -> Result<T, String>,
    from_pcm: impl FnOnce(Pcm) -> T,
) -> Result<T, String> {
    if cancelled() {
        return Err("decode cancelled".into());
    }
    native(path, cancelled).or_else(|e| {
        if cancelled() {
            return Err("decode cancelled".into());
        }
        if crate::video::is_video(path) {
            crate::video::audio::extract(path, cancelled)
                .map_err(|f| format!("{e}; {f}"))
                .and_then(|pcm| {
                    if cancelled() {
                        Err("decode cancelled".into())
                    } else {
                        Ok(from_pcm(pcm))
                    }
                })
        } else {
            Err(e)
        }
    })
}

/// A probed file with its decoder, before the first packet.
struct Native {
    fmt: Box<dyn FormatReader>,
    dec: Box<dyn Decoder>,
    id: u32,
    rate: u32,
    /// Frames the header announces (0 when it does not).
    frames: usize,
}

fn open_native(path: &Path) -> Result<Native, String> {
    let fmt = open_format(path)?;
    let track = audio_track(&*fmt).ok_or("no audio track")?;
    let dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;
    let (id, rate, frames) = (
        track.id,
        track.codec_params.sample_rate.unwrap_or(0),
        track.codec_params.n_frames.unwrap_or(0) as usize,
    );
    Ok(Native {
        fmt,
        dec,
        id,
        rate,
        frames,
    })
}

/// The stereo frames of `samples`, `ch` interleaved channels: mono duplicated, channels past the second dropped.
fn stereo(samples: &[f32], ch: usize) -> impl Iterator<Item = [f32; 2]> {
    samples
        .chunks_exact(ch.max(1))
        .map(move |f| [f[0], f[1.min(ch.saturating_sub(1))]])
}

/// An empty buffer with room for `n` samples when the header's claim can be honoured (a corrupt header must not abort).
fn reserve<T>(n: usize) -> Vec<T> {
    let mut v = Vec::new();
    let _ = v.try_reserve_exact(n);
    v
}

impl Native {
    /// Decodes every packet and hands `push(rate, samples, channels)` its interleaved samples; returns the final rate.
    fn run(
        mut self,
        cancelled: &dyn Fn() -> bool,
        mut push: impl FnMut(u32, &[f32], usize),
    ) -> Result<u32, String> {
        let mut buf: Option<SampleBuffer<f32>> = None;
        let mut any = false;
        loop {
            if cancelled() {
                return Err("decode cancelled".into());
            }
            let pkt = match self.fmt.next_packet() {
                Ok(p) => p,
                Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(SymErr::ResetRequired) => break,
                Err(e) => return Err(format!("read: {e}")),
            };
            if pkt.track_id() != self.id {
                continue;
            }
            let out = match self.dec.decode(&pkt) {
                Ok(o) => o,
                Err(SymErr::DecodeError(_)) => continue, // skip a corrupt frame
                Err(e) => return Err(format!("decode: {e}")),
            };
            let spec = *out.spec();
            self.rate = spec.rate;
            let ch = spec.channels.count();
            let b = buf.get_or_insert_with(|| SampleBuffer::new(out.capacity() as u64, spec));
            b.copy_interleaved_ref(out);
            if ch > 0 && !b.samples().is_empty() {
                any = true;
                push(spec.rate, b.samples(), ch);
            }
        }
        if self.rate == 0 || !any {
            return Err("no audio decoded".into());
        }
        Ok(self.rate)
    }
}

fn decode_native(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Pcm, String> {
    let native = open_native(path)?;
    let mut samples = reserve(2 * native.frames);
    let rate = native.run(cancelled, |_, s, ch| {
        samples.extend(stereo(s, ch).flatten())
    })?;
    Ok(Pcm { rate, samples })
}

fn decode_native_track16(
    path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Track16, String), String> {
    let native = open_native(path)?;
    let mut samples = reserve(2 * native.frames);
    let mut print = None;
    let rate = native.run(cancelled, |rate, s, ch| {
        let print = print.get_or_insert_with(|| Window::new(rate));
        for f in stereo(s, ch) {
            print.push(f);
            samples.extend(f.map(quantise));
        }
    })?;
    Ok((
        Track16::new(rate, samples),
        print.map(Window::finish).unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, n: usize) {
        let data: Vec<u8> = (0..n)
            .flat_map(|i| {
                let v = (f64::sin(2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64)
                    * 16000.0) as i16;
                v.to_le_bytes()
            })
            .collect();
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + data.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend([1, 0, 1, 0]); // PCM, mono
        w.extend(rate.to_le_bytes());
        w.extend((rate * 2).to_le_bytes());
        w.extend([2, 0, 16, 0]);
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(data);
        std::fs::write(path, w).unwrap();
    }

    #[test]
    fn decodes_mono_wav_to_stereo() {
        let p = std::env::temp_dir().join(format!("openfader_audio_{}.wav", std::process::id()));
        write_wav(&p, 22050, 22050);
        let pcm = decode(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(pcm.rate, 22050);
        assert_eq!(pcm.samples.len(), 2 * 22050);
        assert!(pcm.samples.chunks(2).all(|f| f[0] == f[1]));
        assert!(pcm.samples.iter().any(|&s| s.abs() > 0.4));
        assert!((pcm.duration_ms() as i32 - 1000).abs() <= 1);
    }

    #[test]
    fn decode_async_delivers_the_same_pcm_and_reports_errors() {
        let p =
            std::env::temp_dir().join(format!("openfader_audio_async_{}.wav", std::process::id()));
        write_wav(&p, 22050, 22050);
        let got = decode_async(p.clone()).recv().unwrap().unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!((got.rate, got.samples.len()), (22050, 2 * 22050));
        assert!(decode_async(p).recv().unwrap().is_err()); // file is gone now
    }

    #[test]
    fn track16_decode_matches_the_quantised_pcm_and_its_fingerprint() {
        use crate::audio::Frames;
        // 3 s (the whole track is hashed) and 45 s (the 30 s window after 10 s is).
        for secs in [3, 45] {
            let p = std::env::temp_dir().join(format!(
                "openfader_audio_t16_{secs}_{}.wav",
                std::process::id()
            ));
            write_wav(&p, 8000, 8000 * secs);
            let pcm = decode(&p).unwrap();
            let (track, print) = decode_track16_cancellable(&p, &|| false).unwrap();
            std::fs::remove_file(&p).ok();
            let expected = Track16::from(&pcm);
            assert_eq!(track.rate, expected.rate);
            assert_eq!(track.frame_count(), expected.frame_count());
            assert!(
                track
                    .frames_in(0..track.frames())
                    .eq(expected.frames_in(0..track.frames()))
            );
            assert_eq!(print, crate::analysis::fingerprint::of(&pcm));
            assert_eq!(print.len(), 16);
        }
        let gone = std::env::temp_dir().join("openfader_audio_t16_gone.wav");
        assert!(decode_track16_async(gone).recv().unwrap().is_err());
    }

    #[test]
    fn cancellation_stops_between_packets_without_returning_partial_audio() {
        let path =
            std::env::temp_dir().join(format!("openfader_cancel_{}.wav", std::process::id()));
        write_wav(&path, 22050, 22050 * 10);
        let checks = std::cell::Cell::new(0);
        let result = decode_cancellable(&path, &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 4
        });
        assert_eq!(result.err().as_deref(), Some("decode cancelled"));
        let expected = decode(&path).unwrap();
        let actual = decode_cancellable(&path, &|| false).unwrap();
        assert_eq!(actual.rate, expected.rate);
        assert_eq!(actual.samples, expected.samples);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn dropping_a_preview_decode_job_cancels_its_worker() {
        let (tx, rx) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = DecodeJob {
            rx,
            cancelled: cancelled.clone(),
        };
        let (ready_tx, ready_rx) = channel();
        let (resume_tx, resume_rx) = channel();
        let (result_tx, result_rx) = channel();
        let worker = std::thread::spawn(move || {
            let result = decode_cancellable(Path::new("/nonexistent"), &|| {
                ready_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                cancelled.load(Ordering::Relaxed)
            });
            let error = result.as_ref().err().cloned();
            let _ = tx.send(result);
            result_tx.send(error).unwrap();
        });
        ready_rx.recv().unwrap();
        drop(job);
        resume_tx.send(()).unwrap();
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .as_deref(),
            Some("decode cancelled")
        );
        worker.join().unwrap();
    }
}
