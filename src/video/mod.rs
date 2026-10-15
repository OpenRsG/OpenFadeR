//! VIDEO: a deck can hold a video file (mp4 / mov / m4v) as its track. The audio goes through
//! the normal [`crate::audio::decode`] path; the pictures are decoded here, off the UI thread, by an `ffmpeg` child
//! process that streams raw RGB frames over a pipe (no FFmpeg library is linked: the binaries `ffmpeg` / `ffprobe`
//! must be on `PATH`). Each deck's picture follows its audible playhead, so tempo, pause, seek, loops, reverse and
//! scratching need no special cases: the picture is whatever frame the playhead is on.
//!
//! The panel has a master output that can go full screen, a VIDEO CROSSFADER that can be synced to the audio one,
//! and brightness that follows the channel fader. Implemented: the crossfader transition
//! ([`mix::Transition`]: FADE or CUT), the channel-fader brightness, and text / image overlays (drawn by the UI on
//! top of the mixed picture). Camera, transitions beyond fade/cut and linked videos are not implemented.
//!
//! Children: [`clock`] (playhead → frame index), [`probe`] (`ffprobe` parse), [`stream`] (the decode worker: seek, read
//! ahead, a short cache for loops and scratching), [`mix`] (crossfader weights and the blend), [`audio`] (the sound
//! of a video through `ffmpeg`, when the native decoder lacks its codec).

pub mod audio;
pub mod clock;
pub mod mix;
pub mod probe;
pub mod stream;

use std::path::Path;

/// Output picture size: every frame is scaled to fit (letterboxed, never distorted) into this.
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 360;
pub const BYTES: usize = WIDTH * HEIGHT * 3;

/// Container extensions that carry a video track.
pub const EXTS: [&str; 3] = ["mp4", "mov", "m4v"];

pub fn is_video(path: &Path) -> bool {
    crate::paths::has_extension(path, &EXTS)
}

/// One decoded picture, RGB24, [`WIDTH`] × [`HEIGHT`].
pub struct Frame {
    pub rgb: Vec<u8>,
}
