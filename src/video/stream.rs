//! The decode worker of one deck: an `ffmpeg` child process writes raw RGB frames to a pipe and a thread reads them.
//! The UI only calls [`Stream::show`] (one atomic store) and [`Stream::frame`] (a short mutex on an `Arc`); nothing
//! here is reachable from the audio callback.
//!
//! The worker keeps a short run of consecutive frames. A target inside it is served from memory (loops that wrap,
//! reverse play and scratching move around inside it); a target a little ahead is reached by reading on (normal
//! play); anything else (a seek, a cue jump) restarts ffmpeg at the target, a few frames early so that backing up
//! afterwards still hits the cache. Restarts are throttled, so a fast scratch shows fewer pictures, not a fork storm.

use super::{BYTES, Frame, HEIGHT, WIDTH, probe::Info};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::Relaxed};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Frames kept (about 1.5 s at 30 fps, ~1 MB each).
const CACHE: usize = 48;
/// A target this many frames ahead of the cache is read to; further away it is a seek.
const AHEAD: i64 = 45;
/// Frames decoded before a seek target, so that moving backwards stays in the cache.
const BACKFILL: i64 = 12;
/// Frames read beyond the target while playing (hides the decode time of the next one).
const PREFETCH: i64 = 2;
const MIN_RESTART: Duration = Duration::from_millis(60);
const IDLE: Duration = Duration::from_millis(10);
/// `show` value for "no picture" (before the start, past the end, no video).
const NONE: i64 = i64::MIN;

struct Shared {
    target: AtomicI64,
    frame: Mutex<Option<Arc<Frame>>>,
    error: Mutex<Option<String>>,
    stop: AtomicBool,
}

pub struct Stream {
    shared: Arc<Shared>,
    worker: JoinHandle<()>,
}

impl Stream {
    pub fn open(path: PathBuf, info: Info) -> Stream {
        let shared = Arc::new(Shared { target: AtomicI64::new(NONE), frame: Mutex::new(None), error: Mutex::new(None), stop: AtomicBool::new(false) });
        let s = shared.clone();
        let worker = thread::spawn(move || {
            if let Err(e) = run(&s, &path, info) {
                *s.error.lock() = Some(e);
            }
        });
        Stream { shared, worker }
    }

    /// The frame to show (see [`super::clock::frame_index`]); `None` = black.
    pub fn show(&self, index: Option<i64>) {
        let i = index.unwrap_or(NONE);
        if self.shared.target.swap(i, Relaxed) != i {
            self.worker.thread().unpark();
        }
    }

    /// The decoded frame for the last [`Stream::show`], once the worker has it.
    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.shared.frame.lock().clone()
    }

    /// Why the worker stopped (ffmpeg missing, unreadable file), if it did.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().clone()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Relaxed);
        self.worker.thread().unpark(); // the worker kills its child and ends; not joined, so the UI never waits for a frame
    }
}

/// A running ffmpeg and the pipe its frames come out of.
struct Feed {
    child: Child,
    out: ChildStdout,
}

impl Feed {
    /// ffmpeg decoding `path` from frame `from` on at `fps`, scaled to fit [`WIDTH`]×[`HEIGHT`] with black bars.
    fn spawn(path: &Path, from: i64, fps: f64) -> Result<Feed, String> {
        let ss = ((from as f64 - 0.5).max(0.0)) / fps; // half a frame early: the frame at `from` itself must not be cut off
        let vf = format!("fps={fps},scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2:black");
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{ss:.4}"), "-i"])
            .arg(path)
            .args(["-an", "-sn", "-vf", &vf, "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e} (install ffmpeg: video needs the ffmpeg and ffprobe programs)"))?;
        let out = child.stdout.take().ok_or("ffmpeg: no stdout")?;
        Ok(Feed { child, out })
    }

    /// The next frame; `None` at the end of the file.
    fn read(&mut self) -> Option<Arc<Frame>> {
        let mut rgb = vec![0; BYTES];
        self.out.read_exact(&mut rgb).ok()?;
        Some(Arc::new(Frame { rgb }))
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Consecutive decoded frames `first..first + frames.len()`.
#[derive(Default)]
struct Cache {
    first: i64,
    frames: VecDeque<Arc<Frame>>,
}

impl Cache {
    fn end(&self) -> i64 {
        self.first + self.frames.len() as i64 // the next index to arrive
    }

    fn get(&self, i: i64) -> Option<&Arc<Frame>> {
        usize::try_from(i - self.first).ok().and_then(|k| self.frames.get(k))
    }

    fn push(&mut self, f: Arc<Frame>) {
        self.frames.push_back(f);
        if self.frames.len() > CACHE {
            self.frames.pop_front();
            self.first += 1;
        }
    }
}

fn run(sh: &Shared, path: &Path, info: Info) -> Result<(), String> {
    let (mut cache, mut feed, mut eof) = (Cache::default(), None::<Feed>, false);
    let mut published: Option<Option<i64>> = None; // what `frame` holds: Some(None) = black
    let mut restarted = Instant::now().checked_sub(MIN_RESTART).unwrap_or_else(Instant::now);
    while !sh.stop.load(Relaxed) {
        let target = sh.target.load(Relaxed);
        // Past the end of what ffmpeg delivered: the last frame stays.
        let want = if eof && !cache.frames.is_empty() { target.min(cache.end() - 1) } else { target };
        if target == NONE || (eof && cache.frames.is_empty() && want >= cache.first) {
            // Nothing to show (no picture wanted, or the seek landed past the last frame): black until the target moves.
            if published != Some(None) {
                *sh.frame.lock() = None;
                published = Some(None);
            }
        } else if let Some(f) = cache.get(want) {
            if published != Some(Some(want)) {
                *sh.frame.lock() = Some(f.clone());
                published = Some(Some(want));
            }
            if feed.is_some() && cache.end() < want + PREFETCH {
                read_one(&mut feed, &mut cache, &mut eof);
                continue;
            }
        } else if feed.is_some() && want >= cache.end() && want < cache.end() + AHEAD {
            read_one(&mut feed, &mut cache, &mut eof);
            continue;
        } else if restarted.elapsed() >= MIN_RESTART {
            // A seek (or the first frame): restart a little before the target.
            let from = (target - BACKFILL).max(0);
            drop(feed.take());
            feed = Some(Feed::spawn(path, from, info.fps)?);
            (cache, eof, restarted) = (Cache { first: from, frames: VecDeque::new() }, false, Instant::now());
            continue;
        }
        thread::park_timeout(IDLE);
    }
    Ok(())
}

fn read_one(feed: &mut Option<Feed>, cache: &mut Cache, eof: &mut bool) {
    match feed.as_mut().and_then(Feed::read) {
        Some(f) => cache.push(f),
        None => {
            *feed = None; // ends the child; the cache stays for loops and for holding the last frame
            *eof = true;
        }
    }
}
