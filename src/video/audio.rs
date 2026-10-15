//! The audio of a video file through `ffmpeg`, for codecs the native decoder lacks (AC-3, Opus, …): stereo f32 at [`RATE`].

use crate::audio::Pcm;
use std::path::Path;
use std::process::{Command, Stdio};

pub const RATE: u32 = 44_100;

pub fn extract(path: &Path) -> Result<Pcm, String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", &RATE.to_string(), "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.len() < 8 {
        return Err(format!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let samples = out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    Ok(Pcm { rate: RATE, samples })
}
