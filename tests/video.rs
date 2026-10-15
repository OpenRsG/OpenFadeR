//! VIDEO against a synthetic clip made with the `ffmpeg` binary (skipped when it is not on PATH): the picture's
//! brightness is its time (`lum = 60 · T`), so the frame on screen can be read off the pixels. Audio goes through the
//! normal decoder.

use openfader::video::{self, HEIGHT, WIDTH, clock, probe, stream::Stream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Per-process temp dir for the clips (removed with the OS temp cleanup).
static DIR: LazyLock<PathBuf> = LazyLock::new(|| std::env::temp_dir().join(format!("openfader-video-test-{}", std::process::id())));
static GENERATING: Mutex<()> = Mutex::new(()); // tests run in parallel and share the clips

/// 4 s, 25 fps, `size`, a 440 Hz tone; `None` without ffmpeg.
fn clip(name: &str, size: &str) -> Option<PathBuf> {
    let _lock = GENERATING.lock().unwrap_or_else(|e| e.into_inner());
    let dir = &*DIR;
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    if !path.exists() {
        let src = format!("color=c=black:s={size}:r=25:d=4,geq=lum='min(255,T*60)':cb=128:cr=128");
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &src, "-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", if name.starts_with("ac3") { "ac3" } else { "aac" }, "-shortest"])
            .arg(&path)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
    }
    Some(path)
}

/// Luma the clip shows at frame `i`, as it comes out of the RGB conversion.
fn luma(i: i64) -> f64 {
    ((i as f64 * 2.4 - 16.0) * 255.0 / 219.0).clamp(0.0, 255.0)
}

/// Shows frame `i` and waits until the centre pixel is the one frame `i` has (±3 frames of brightness).
fn show(s: &Stream, i: i64) -> Vec<u8> {
    s.show(Some(i));
    let t = Instant::now();
    loop {
        if let Some(f) = s.frame() {
            assert_eq!(f.rgb.len(), video::BYTES);
            let px = f.rgb[(HEIGHT / 2 * WIDTH + WIDTH / 2) * 3];
            if (f64::from(px) - luma(i)).abs() <= 8.0 {
                return f.rgb.clone();
            }
        }
        assert!(t.elapsed() < Duration::from_secs(8), "frame {i} never showed (stream error: {:?})", s.error());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open(path: &Path) -> Stream {
    Stream::open(path.to_owned(), probe::probe(path).unwrap())
}

macro_rules! need_clip {
    ($name:expr, $size:expr) => {
        match clip($name, $size) {
            Some(p) => p,
            None => return eprintln!("skipped: no ffmpeg"),
        }
    };
}

#[test]
fn probe_reads_the_clip() {
    let p = need_clip!("a.mp4", "64x36");
    let i = probe::probe(&p).unwrap();
    assert_eq!((i.fps, i.duration_ms, i.frames()), (25.0, 4000, 100));
    assert!(video::is_video(&p));
}

#[test]
fn the_audio_of_a_video_decodes_like_any_track() {
    let p = need_clip!("a.mp4", "64x36");
    let pcm = openfader::audio::decode(&p).unwrap();
    let secs = pcm.samples.len() as f64 / 2.0 / f64::from(pcm.rate);
    assert!((3.8..4.3).contains(&secs), "{secs} s");
}

#[test]
fn frames_follow_the_playhead_through_seeks_loops_and_the_end() {
    let p = need_clip!("a.mp4", "64x36");
    let s = open(&p);
    show(&s, 0);
    show(&s, 25); // seek
    // normal play: frame by frame at the clip's pace
    for i in 26..60 {
        show(&s, i);
    }
    show(&s, 90); // jump ahead
    show(&s, 20); // jump back
    // a loop wraps back inside the cache, and a scratch backs up by a few frames
    for i in 21..40 {
        show(&s, i);
    }
    for i in (30..40).rev() {
        show(&s, i);
    }
    // past the last frame the last one stays; no picture asked for = black
    show(&s, 99);
    let last = show(&s, 150);
    assert_eq!(last, s.frame().unwrap().rgb);
    s.show(None);
    let t = Instant::now();
    while s.frame().is_some() {
        assert!(t.elapsed() < Duration::from_secs(2), "frame not cleared");
        std::thread::sleep(Duration::from_millis(5));
    }
    // the playhead → frame mapping lands on the same pixels
    let f = clock::frame_index(clock::video_time(1000.0, 0.0), 25.0, 100).unwrap();
    assert_eq!(f, 25);
    show(&s, f);
}

#[test]
fn a_narrow_picture_is_letterboxed_not_stretched() {
    let p = need_clip!("b.mp4", "48x36"); // 4:3 into 16:9: bars left and right
    let s = open(&p);
    let rgb = show(&s, 50);
    let px = |x: usize, y: usize| rgb[(y * WIDTH + x) * 3];
    assert!(px(5, HEIGHT / 2) <= 2, "left bar {}", px(5, HEIGHT / 2));
    assert!(px(WIDTH - 5, HEIGHT / 2) <= 2);
    assert!((i32::from(px(WIDTH / 2, HEIGHT / 2)) - 119).abs() <= 8);
}

#[test]
fn a_folder_library_lists_video_files_as_tracks() {
    let p = need_clip!("c.mp4", "64x36");
    let root = DIR.join("lib");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(&p, root.join("clip.mp4")).unwrap();
    let tracks = openfader::library::open(&root).unwrap().tracks(None).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!((tracks[0].title.as_str(), tracks[0].length_s), ("clip", 4));
    assert!(video::is_video(tracks[0].file.as_deref().unwrap()));
}

#[test]
fn audio_symphonia_cannot_decode_comes_through_ffmpeg() {
    let p = need_clip!("ac3.mp4", "64x36"); // AC-3: no native decoder
    let pcm = openfader::audio::decode(&p).unwrap();
    let secs = pcm.samples.len() as f64 / 2.0 / f64::from(pcm.rate);
    assert!((3.8..4.4).contains(&secs), "{secs} s");
    assert!(pcm.samples.iter().any(|s| s.abs() > 0.05), "silent");
}
