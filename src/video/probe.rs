//! `ffprobe` of a video file: size, frame rate and length of its first video stream.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    pub fps: f64,
    pub duration_ms: u32,
}

impl Info {
    /// Frames in the file (the ones [`super::clock::frame_index`] may name).
    pub fn frames(&self) -> i64 {
        (self.duration_ms as f64 * self.fps / 1000.0).round() as i64
    }
}

/// Frame rates above this are decoded at this rate (the output is a preview-sized picture mixed at the UI's pace).
const MAX_FPS: f64 = 60.0;

pub fn probe(path: &Path) -> Result<Info, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate,r_frame_rate,duration:format=duration", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e} (install ffmpeg: video needs the ffmpeg and ffprobe programs)"))?;
    if !out.status.success() {
        return Err(format!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    parse(&String::from_utf8_lossy(&out.stdout))
}

/// `key=value` lines as ffprobe prints them. The stream's `duration` comes first, the container's after it (either may be `N/A`).
fn parse(text: &str) -> Result<Info, String> {
    let get = |key: &str| text.lines().filter_map(|l| l.strip_prefix(key)?.strip_prefix('=')).collect::<Vec<_>>();
    let rate = |s: &&str| {
        let (n, d) = s.split_once('/')?;
        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
        (n > 0.0 && d > 0.0).then_some(n / d)
    };
    let fps = get("avg_frame_rate").iter().chain(&get("r_frame_rate")).find_map(rate).ok_or("no video stream")?;
    let secs = get("duration").iter().find_map(|s| s.parse::<f64>().ok().filter(|d| *d > 0.0)).ok_or("video has no duration")?;
    Ok(Info { fps: fps.min(MAX_FPS), duration_ms: (secs * 1000.0).round() as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_output() {
        let i = parse("avg_frame_rate=30000/1001\nr_frame_rate=30000/1001\nduration=N/A\nduration=12.512000\n").unwrap();
        assert!((i.fps - 29.97).abs() < 0.01);
        assert_eq!(i.duration_ms, 12512);
        assert_eq!(i.frames(), 375);
        // average rate 0/0 (some containers): the nominal rate; a fast file is capped
        let i = parse("avg_frame_rate=0/0\nr_frame_rate=120/1\nduration=2.0\nduration=2.0\n").unwrap();
        assert_eq!((i.fps, i.frames()), (60.0, 120));
    }

    #[test]
    fn no_video_stream_is_an_error() {
        assert_eq!(parse("duration=3.0\n"), Err("no video stream".into()));
        assert_eq!(parse("avg_frame_rate=25/1\nduration=N/A\nduration=N/A\n"), Err("video has no duration".into()));
    }
}
