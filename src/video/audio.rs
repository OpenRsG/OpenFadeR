//! The audio of a video file through `ffmpeg`, for codecs the native decoder lacks (AC-3, Opus, …): stereo f32 at [`RATE`].

use crate::audio::Pcm;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub const RATE: u32 = 44_100;

pub fn extract(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Pcm, String> {
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-f",
            "f32le",
            "-ac",
            "2",
            "-ar",
            &RATE.to_string(),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg: no audio pipe")?;
    let mut stderr = child.stderr.take().ok_or("ffmpeg: no error pipe")?;
    std::thread::scope(|scope| {
        let output = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let errors = scope.spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let status = loop {
            if cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("decode cancelled".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("ffmpeg: {e}"));
                }
            }
        };
        let bytes = output
            .join()
            .map_err(|_| "ffmpeg: audio reader stopped")?
            .map_err(|e| format!("ffmpeg: {e}"))?;
        let errors = errors.join().map_err(|_| "ffmpeg: error reader stopped")?;
        if !status.success() || bytes.len() < 8 {
            return Err(format!(
                "ffmpeg: {}",
                String::from_utf8_lossy(&errors).trim()
            ));
        }
        let samples = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(Pcm {
            rate: RATE,
            samples,
        })
    })
}
