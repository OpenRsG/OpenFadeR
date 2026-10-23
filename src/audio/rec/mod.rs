//! REC: the post-master stereo mix ([`Tap`]) to 16-bit stereo PCM WAV files, written off the audio thread.
//!
//! The audio callback only pushes frames into the tap's lock-free [`super::Ring`]; a writer thread
//! drains it every 10 ms through the silence [`gate`] (start trigger, auto-separation, end trigger) into a
//! `BufWriter<File>`. The WAV header ([`wav`]) starts as that of an empty file and is patched about once a
//! second and on close, so a crash loses at most the last second of the length field, not the file.
//! ponytail: the sizes are u32, so a file past 4 GiB (~6 h at 48 kHz) has a saturated header; the
//! auto-separation is the only splitter.
//!
//! Children: [`wav`] (the writer), [`gate`] (pure silence state machine, [`Options`]), [`optimise`] (level
//! optimisation of a finished file).

mod gate;
mod optimise;
mod wav;

pub use gate::Options;

use super::Tap;
use gate::{Gate, Step};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wav::Wav;

const POLL: Duration = Duration::from_millis(10);
const PATCH_EVERY: Duration = Duration::from_secs(1);

/// What a finished recording holds.
#[derive(Debug, PartialEq)]
pub struct Take {
    pub frames: u64,
    pub rate: u32,
    /// Frames the audio thread had to drop because the writer fell behind (0 normally).
    pub dropped: u64,
    /// The files written, in order (auto-separation makes more than one; none when no audio came).
    pub files: Vec<PathBuf>,
    /// Where each of `files` begins on the recording's own clock, in frames (the first is 0): the frames written before it.
    pub starts: Vec<u64>,
}

impl Take {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames as f64 / self.rate as f64)
    }
}

/// Chooses the path of the next file; it must not exist.
pub type NewPath = Box<dyn FnMut() -> io::Result<PathBuf> + Send>;

/// What the writer thread returns: the frames written, the files and where each began.
type Done = (u64, Vec<PathBuf>, Vec<u64>);

/// The files of one recording: opens them on demand, patches the header, optimises on close.
struct Writer {
    rate: u32,
    new_path: NewPath,
    optimise: bool,
    open: Option<(Wav<BufWriter<File>>, PathBuf)>,
    files: Vec<PathBuf>,
    /// The frame count when each of `files` was opened.
    starts: Vec<u64>,
    opened_at: u64,
    frames: u64,
    /// The file being written, for [`Recorder::path`].
    current: Arc<Mutex<Option<PathBuf>>>,
}

impl Writer {
    fn open(&mut self) -> io::Result<()> {
        let path = (self.new_path)()?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut wav = Wav::new(BufWriter::new(file), self.rate)?;
        wav.sync()?;
        self.opened_at = self.frames;
        *self.current.lock().expect("path lock") = Some(path.clone());
        self.open = Some((wav, path));
        Ok(())
    }

    fn write(&mut self, frames: &[[f32; 2]]) -> io::Result<()> {
        if self.open.is_none() {
            self.open()?;
        }
        if let Some((wav, _)) = &mut self.open {
            wav.write(frames)?;
        }
        self.frames += frames.len() as u64;
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.open.as_mut().map_or(Ok(()), |(w, _)| w.sync())
    }

    fn close(&mut self) -> io::Result<()> {
        if let Some((wav, path)) = self.open.take() {
            wav.finish()?;
            if self.optimise {
                optimise::optimise(&path)?;
            }
            self.files.push(path);
            self.starts.push(self.opened_at);
        }
        Ok(())
    }
}

/// A running recording. Dropping it stops and finalises the file like [`Recorder::stop`].
pub struct Recorder {
    tap: Arc<Tap>,
    written: Arc<AtomicU64>,
    current: Arc<Mutex<Option<PathBuf>>>,
    dropped0: u64,
    thread: Option<JoinHandle<io::Result<Done>>>,
}

impl Recorder {
    /// Starts recording the tap's frames. With the REC-button start trigger the first file is created now
    /// (an error is returned here); with audio detection it is created when the audio arrives.
    pub fn start(tap: Arc<Tap>, options: Options, new_path: NewPath) -> io::Result<Recorder> {
        let gate = Gate::new(options, tap.rate());
        let current = Arc::new(Mutex::new(None));
        let mut writer = Writer {
            rate: tap.rate(),
            new_path,
            optimise: options.optimise,
            open: None,
            files: Vec::new(),
            starts: Vec::new(),
            opened_at: 0,
            frames: 0,
            current: current.clone(),
        };
        if gate.open_at_start() {
            writer.open()?;
        }
        while tap.ring.pop().is_some() {} // frames a previous recording left behind
        let written = Arc::new(AtomicU64::new(0));
        let dropped0 = tap.ring.dropped();
        tap.on.store(true, Release);
        let thread = {
            let (tap, written) = (tap.clone(), written.clone());
            std::thread::Builder::new()
                .name("rec-writer".into())
                .spawn(move || {
                    let r = drain(&tap, gate, writer, &written);
                    tap.on.store(false, Release); // on a write error or the end trigger too: stop feeding a dead writer
                    r
                })?
        };
        Ok(Recorder {
            tap,
            written,
            current,
            dropped0,
            thread: Some(thread),
        })
    }

    /// Recorded time so far (frames on their way to disk are not counted yet).
    pub fn elapsed(&self) -> Duration {
        Duration::from_secs_f64(self.written.load(Relaxed) as f64 / self.tap.rate() as f64)
    }

    /// The file being written, `None` while the start trigger waits for audio.
    pub fn path(&self) -> Option<PathBuf> {
        self.current.lock().expect("path lock").clone()
    }

    /// The writer thread ended by itself: an I/O error, or the end trigger (silence); [`Recorder::stop`] returns which.
    pub fn failed(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| t.is_finished())
    }

    /// Stops feeding, writes what is queued, finalises the header and closes the file.
    pub fn stop(mut self) -> io::Result<Take> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<Take> {
        self.tap.on.store(false, Release);
        let t = self.thread.take().expect("stopped once");
        let (frames, files, starts) = t
            .join()
            .map_err(|_| io::Error::other("writer thread panicked"))??;
        Ok(Take {
            frames,
            rate: self.tap.rate(),
            dropped: self.tap.ring.dropped() - self.dropped0,
            files,
            starts,
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.finish();
        }
    }
}

/// Writer thread: drain the ring until the tap is switched off *and* empty (or the gate ends the recording).
/// `on` is read before the drain so frames pushed up to the switch-off are written.
fn drain(tap: &Tap, mut gate: Gate, mut w: Writer, written: &AtomicU64) -> io::Result<Done> {
    let mut buf = Vec::with_capacity(4096);
    let mut patched = Instant::now();
    loop {
        let live = tap.on.load(Acquire);
        loop {
            buf.clear();
            buf.extend(std::iter::from_fn(|| tap.ring.pop()).take(4096));
            if buf.is_empty() {
                break;
            }
            for step in gate.feed(&buf) {
                match step {
                    Step::Write(r) => w.write(&buf[r])?,
                    Step::Split => w.close()?,
                    Step::End => {
                        w.close()?;
                        written.store(w.frames, Relaxed);
                        return Ok((w.frames, w.files, w.starts));
                    }
                }
            }
            written.store(w.frames, Relaxed);
        }
        if !live {
            w.close()?;
            return Ok((w.frames, w.files, w.starts));
        }
        if patched.elapsed() >= PATCH_EVERY {
            w.sync()?;
            patched = Instant::now();
        }
        std::thread::sleep(POLL);
    }
}

/// `<dir>/rec-YYYYmmdd-HHMMSS.wav` (local time) in `dir` (Preferences > Advanced > Recordings > Location of
/// Files), else in `$OPENFADER_REC_DIR` (see [`crate::paths::output_dir`]); the folder is created.
pub fn new_path(dir: Option<&Path>) -> io::Result<PathBuf> {
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| crate::paths::output_dir("OPENFADER_REC_DIR"))
        .ok_or_else(|| io::Error::other("no recording directory: set OPENFADER_REC_DIR or HOME"))?;
    crate::paths::timestamped(&dir, "rec", "wav")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wavout::u32_at;

    fn fixed(path: PathBuf) -> NewPath {
        Box::new(move || Ok(path.clone()))
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openfader-rec-{name}-{}", std::process::id()))
    }

    fn feed(tap: &Tap, frames: impl Iterator<Item = [f32; 2]>) {
        for f in frames {
            while !tap.ring.push(f) {
                std::thread::yield_now();
            }
        }
    }

    const NOW: Options = Options {
        start_on_audio: false,
        end_on_silence: false,
        threshold_db: -60.0,
        split_after_s: None,
        optimise: false,
    };

    #[test]
    fn recorder_writes_exactly_the_frames_fed() {
        let path = tmp("one.wav");
        let _ = std::fs::remove_file(&path);
        let tap = Arc::new(Tap::new(48000, 1024)); // far smaller than the feed: the test paces itself
        let rec = Recorder::start(tap.clone(), NOW, fixed(path.clone())).unwrap();
        assert_eq!(
            rec.path(),
            Some(path.clone()),
            "the REC-button start creates the file at once"
        );
        let n = 50_000usize;
        let val = |k: usize| ((k % 2000) as f32 - 1000.0) / 1000.0;
        feed(&tap, (0..n).map(|k| [val(k), -val(k)]));
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            rec.elapsed() > Duration::ZERO,
            "progress is visible while recording"
        );
        let take = rec.stop().unwrap();
        assert!(!tap.on.load(Acquire));
        assert_eq!(
            (take.frames, take.rate, take.files.clone()),
            (n as u64, 48000, vec![path.clone()])
        );
        let b = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(b.len(), 44 + 4 * n);
        assert_eq!(u32_at(&b, 40) as usize, 4 * n);
        let s = |i: usize| i16::from_le_bytes([b[44 + 2 * i], b[45 + 2 * i]]);
        for k in [0, 1, 999, 1000, 49_999] {
            let want = (
                (val(k).clamp(-1.0, 1.0) * 32767.0).round() as i16,
                (-val(k) * 32767.0).round() as i16,
            );
            assert_eq!((s(2 * k), s(2 * k + 1)), want, "frame {k}");
        }
    }

    #[test]
    fn a_second_recording_does_not_overwrite_the_first() {
        let path = tmp("two.wav");
        let _ = std::fs::remove_file(&path);
        let tap = Arc::new(Tap::new(8000, 64));
        Recorder::start(tap.clone(), NOW, fixed(path.clone()))
            .unwrap()
            .stop()
            .unwrap();
        let again = Recorder::start(tap, NOW, fixed(path.clone()));
        assert_eq!(
            again.err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn audio_start_with_separation_writes_one_file_per_take_and_optimises_each() {
        let dir = tmp("split");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tap = Arc::new(Tap::new(1000, 1024));
        let opts = Options {
            start_on_audio: true,
            end_on_silence: false,
            threshold_db: -60.0,
            split_after_s: Some(1),
            optimise: true,
        };
        let n = std::cell::Cell::new(0);
        let d = dir.clone();
        let next: NewPath = Box::new(move || {
            n.set(n.get() + 1);
            Ok(d.join(format!("t{}.wav", n.get())))
        });
        let rec = Recorder::start(tap.clone(), opts, next).unwrap();
        assert_eq!(rec.path(), None, "armed: no file before audio");
        let q = [0.0f32; 2];
        let l = [0.1f32, -0.1];
        feed(
            &tap,
            std::iter::repeat_n(q, 500)
                .chain(std::iter::repeat_n(l, 2000))
                .chain(std::iter::repeat_n(q, 3000))
                .chain(std::iter::repeat_n(l, 1500)),
        );
        std::thread::sleep(Duration::from_millis(50));
        let take = rec.stop().unwrap();
        assert_eq!(take.files, [dir.join("t1.wav"), dir.join("t2.wav")]);
        let len = |p: &Path| (std::fs::metadata(p).unwrap().len() as usize - 44) / 4;
        assert_eq!(
            len(&take.files[0]),
            2000 + 1000,
            "the take and the second of silence that separated it"
        );
        assert_eq!(len(&take.files[1]), 1500);
        assert_eq!(take.starts, [0, 3000], "each file's first frame on the recording's clock");
        let peak = std::fs::read(&take.files[1]).unwrap()[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 32000, "optimised to full scale: {peak}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn end_trigger_stops_the_writer_by_itself() {
        let dir = tmp("end");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tap = Arc::new(Tap::new(100, 1024));
        let opts = Options {
            start_on_audio: true,
            end_on_silence: true,
            threshold_db: -60.0,
            split_after_s: None,
            optimise: false,
        };
        let rec = Recorder::start(tap.clone(), opts, fixed(dir.join("a.wav"))).unwrap();
        feed(
            &tap,
            std::iter::repeat_n([0.2f32; 2], 50).chain(std::iter::repeat_n([0.0f32; 2], 2100)),
        );
        for _ in 0..200 {
            if rec.failed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(rec.failed(), "20 s of silence ended the recording");
        let take = rec.stop().unwrap();
        assert_eq!(
            take.frames,
            50 + 2000,
            "audio plus the 20 s of silence that ended it"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
