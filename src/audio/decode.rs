//! Whole-file decode (symphonia).

use super::Pcm;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// [`decode`] on a worker thread; the receiver gets exactly one result.
pub fn decode_async(path: PathBuf) -> Receiver<Result<Pcm, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(decode(&path)); // receiver dropped = load cancelled
    });
    rx
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
/// ponytail: whole-file decode costs ~10 MB/min of RAM; upgrade path is a streaming decoder thread feeding a ring buffer.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    decode_native(path).or_else(|e| {
        if crate::video::is_video(path) {
            crate::video::audio::extract(path).map_err(|f| format!("{e}; {f}"))
        } else {
            Err(e)
        }
    })
}

fn decode_native(path: &Path) -> Result<Pcm, String> {
    let mut fmt = open_format(path)?;
    let track = audio_track(&*fmt).ok_or("no audio track")?;
    let id = track.id;
    let mut dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut samples = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let pkt = match fmt.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => return Err(format!("read: {e}")),
        };
        if pkt.track_id() != id {
            continue;
        }
        let out = match dec.decode(&pkt) {
            Ok(o) => o,
            Err(SymErr::DecodeError(_)) => continue, // skip a corrupt frame
            Err(e) => return Err(format!("decode: {e}")),
        };
        let spec = *out.spec();
        rate = spec.rate;
        let ch = spec.channels.count();
        let b = buf.get_or_insert_with(|| SampleBuffer::new(out.capacity() as u64, spec));
        b.copy_interleaved_ref(out);
        match ch {
            0 => {}
            1 => samples.extend(b.samples().iter().flat_map(|&s| [s, s])),
            2 => samples.extend_from_slice(b.samples()),
            _ => samples.extend(b.samples().chunks_exact(ch).flat_map(|f| [f[0], f[1]])),
        }
    }
    if rate == 0 || samples.is_empty() {
        return Err("no audio decoded".into());
    }
    Ok(Pcm { rate, samples })
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
}
