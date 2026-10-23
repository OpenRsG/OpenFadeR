//! The log file: `openfader.log` in the logs folder, a timestamped line per line the app printed, rotated by size.
//!
//! [`Log`] is the file (buffered, `openfader.log.1` … `.N` are the older ones) plus the last lines kept for reports.
//! [`tee`] makes what the process writes to its stdout / stderr also arrive here: the descriptor (the handle on Windows) is
//! replaced by a pipe, a thread forwards each chunk to the original and gives its lines to the [`Log`]. The audio thread never
//! writes a file: it writes to a pipe, like it did to the terminal.

use chrono::Local;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Lines kept for the report (the window and the crash file show these).
pub const TAIL: usize = 300;
/// A stored or written line is cut here (a runaway print must not fill the file).
const LINE_MAX: usize = 4000;

pub struct Log {
    path: PathBuf,
    cap: u64,
    keep: usize,
    file: BufWriter<File>,
    size: u64,
    tail: VecDeque<String>,
}

impl Log {
    /// `dir/openfader.log` opened for appending; one already over `cap` bytes is rotated first. `keep` older files are kept.
    pub fn open(dir: &Path, cap: u64, keep: usize) -> io::Result<Log> {
        fs::create_dir_all(dir)?;
        let path = dir.join("openfader.log");
        let (file, size) = append(&path)?;
        let mut log = Log {
            path,
            cap,
            keep,
            file,
            size,
            tail: VecDeque::new(),
        };
        if size >= cap {
            log.rotate()?;
        }
        Ok(log)
    }

    /// One line: `YYYY-MM-DD HH:MM:SS.mmm [tag] text`, written and kept for [`Log::tail`].
    pub fn line(&mut self, tag: &str, text: &str) {
        let text = text.trim_end_matches(['\r', '\n']);
        let text = text
            .char_indices()
            .nth(LINE_MAX)
            .map_or(text, |(n, _)| &text[..n]);
        let line = format!(
            "{} [{tag}] {text}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
        );
        if self.size >= self.cap {
            let _ = self.rotate();
        }
        if writeln!(self.file, "{line}").is_ok() {
            self.size += line.len() as u64 + 1;
        }
        if self.tail.len() == TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line);
    }

    pub fn flush(&mut self) {
        let _ = self.file.flush();
    }

    /// The last [`TAIL`] lines, oldest first.
    pub fn tail(&self) -> Vec<String> {
        self.tail.iter().cloned().collect()
    }

    /// `openfader.log` becomes `.1`, `.1` becomes `.2` …; the one past `keep` is deleted.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let old = |n: usize| PathBuf::from(format!("{}.{n}", self.path.display()));
        let _ = fs::remove_file(old(self.keep));
        for n in (1..self.keep).rev() {
            let _ = fs::rename(old(n), old(n + 1));
        }
        if self.keep > 0 {
            fs::rename(&self.path, old(1))?;
        } else {
            fs::remove_file(&self.path)?;
        }
        (self.file, self.size) = append(&self.path)?;
        Ok(())
    }
}

fn append(path: &Path) -> io::Result<(BufWriter<File>, u64)> {
    let f = OpenOptions::new().create(true).append(true).open(path)?;
    let size = f.metadata()?.len();
    Ok((BufWriter::new(f), size))
}

/// Splits a byte stream into lines; a partial line waits for the next chunk (or [`LINE_MAX`] bytes).
#[derive(Default)]
pub struct Lines(Vec<u8>);

impl Lines {
    pub fn feed(&mut self, chunk: &[u8], mut each: impl FnMut(&str)) {
        self.0.extend_from_slice(chunk);
        loop {
            let end = match self.0.iter().position(|&b| b == b'\n') {
                Some(n) => n,
                None if self.0.len() >= LINE_MAX => self.0.len(),
                None => break,
            };
            let line: Vec<u8> = self.0.drain(..(end + 1).min(self.0.len())).collect();
            each(&String::from_utf8_lossy(&line[..end]));
        }
    }

    pub fn rest(&mut self, mut each: impl FnMut(&str)) {
        if !self.0.is_empty() {
            each(&String::from_utf8_lossy(&std::mem::take(&mut self.0)));
        }
    }
}

/// A redirected stream: [`Tee::stop`] puts the original back.
pub struct Tee {
    restore: Box<dyn FnOnce() + Send>,
    done: mpsc::Receiver<()>,
}

impl Tee {
    /// Puts the original stream back and waits (briefly) until the thread has logged what was still in the pipe.
    pub fn stop(self) {
        (self.restore)();
        let _ = self.done.recv_timeout(Duration::from_millis(500));
    }
}

/// Starts the thread behind a redirected stream: bytes from `r` go to `orig` (when there is one) and, as lines tagged `tag`, to `log`.
fn forward(
    mut r: impl Read + Send + 'static,
    mut orig: Option<File>,
    tag: &'static str,
    log: &'static Mutex<Option<Log>>,
) -> io::Result<mpsc::Receiver<()>> {
    let (tx, done) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("log-{tag}"))
        .spawn(move || {
            let (mut buf, mut lines) = ([0u8; 8192], Lines::default());
            loop {
                let n = match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if let Some(f) = orig.as_mut() {
                    let _ = f.write_all(&buf[..n]).and_then(|()| f.flush());
                }
                let mut guard = log.lock();
                if let Some(l) = guard.as_mut() {
                    lines.feed(&buf[..n], |s| l.line(tag, s));
                    l.flush();
                }
            }
            if let Some(l) = log.lock().as_mut() {
                lines.rest(|s| l.line(tag, s));
                l.flush();
            }
            drop(tx);
        })?;
    Ok(done)
}

/// Redirects stdout (`tag` "out") or stderr ("err") through [`forward`].
#[cfg(unix)]
pub fn tee(err: bool, log: &'static Mutex<Option<Log>>) -> io::Result<Tee> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = if err { 2 } else { 1 };
    let (r, w) = io::pipe()?;
    // SAFETY: plain descriptor calls; `saved` is a fresh descriptor this function owns from here on.
    let saved = unsafe { libc::dup(fd) };
    let orig = (saved >= 0).then(|| {
        // Not inherited by the helper programs the app starts.
        unsafe { libc::fcntl(saved, libc::F_SETFD, libc::FD_CLOEXEC) };
        unsafe { File::from_raw_fd(saved) }
    });
    if unsafe { libc::dup2(w.as_raw_fd(), fd) } < 0 {
        return Err(io::Error::last_os_error());
    }
    drop(w);
    let done = forward(
        r,
        orig.as_ref().and_then(|f| f.try_clone().ok()),
        if err { "err" } else { "out" },
        log,
    )?;
    let restore = Box::new(move || {
        if let Some(f) = &orig {
            // SAFETY: puts the saved descriptor back on `fd`; the pipe's last writer closes, the thread reads to its end.
            unsafe { libc::dup2(f.as_raw_fd(), fd) };
        }
    });
    Ok(Tee { restore, done })
}

#[cfg(windows)]
pub fn tee(err: bool, log: &'static Mutex<Option<Log>>) -> io::Result<Tee> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    let which = if err {
        STD_ERROR_HANDLE
    } else {
        STD_OUTPUT_HANDLE
    };
    let (r, w) = io::pipe()?;
    let w = OwnedHandle::from(w);
    // SAFETY: the handle is the process's own; a null or invalid one (no console behind a windowed program) is not wrapped.
    let old = unsafe { GetStdHandle(which) };
    let orig = (!old.is_null() && old as isize != -1)
        .then(|| unsafe { File::from_raw_handle(old as RawHandle) });
    if unsafe { SetStdHandle(which, w.as_raw_handle() as _) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let done = forward(
        r,
        orig.as_ref().and_then(|f| f.try_clone().ok()),
        if err { "err" } else { "out" },
        log,
    )?;
    let restore = Box::new(move || {
        // SAFETY: the original handle goes back; the pipe's writer closes with `w`.
        unsafe { SetStdHandle(which, old) };
        drop((w, orig));
    });
    Ok(Tee { restore, done })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openfader-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_log_rotates_at_its_size_keeps_a_few_old_files_and_drops_the_oldest() {
        let d = dir("rotate");
        let mut log = Log::open(&d, 300, 2).unwrap();
        for i in 0..40 {
            log.line("out", &format!("line number {i:02}"));
        }
        log.flush();
        let names = |p: &str| fs::read_to_string(d.join(p)).unwrap_or_default();
        assert!(d.join("openfader.log.1").exists() && d.join("openfader.log.2").exists());
        assert!(
            !d.join("openfader.log.3").exists(),
            "only 2 old files are kept"
        );
        let (now, one, two) = (
            names("openfader.log"),
            names("openfader.log.1"),
            names("openfader.log.2"),
        );
        assert!(now.contains("line number 39") && !now.contains("line number 00"));
        assert!(
            one.len() < 400 && two.len() < 400,
            "each file stays near the cap"
        );
        assert!(
            !format!("{now}{one}{two}").contains("line number 00"),
            "the oldest lines are gone"
        );
        assert!(
            now.lines()
                .all(|l| l.contains(" [out] line number ") && l.as_bytes()[4] == b'-'),
            "timestamped: {now}"
        );
        assert_eq!(
            log.tail().len(),
            40,
            "the report's tail is not cut by rotation"
        );
        // A log already over the cap rotates when it is opened.
        drop(log);
        let log = Log::open(&d, 10, 2).unwrap();
        assert_eq!(fs::metadata(d.join("openfader.log")).unwrap().len(), 0);
        drop(log);
    }

    #[test]
    fn a_pipe_is_cut_into_lines_with_the_partial_one_kept_for_the_next_chunk() {
        let mut got = Vec::new();
        let mut l = Lines::default();
        l.feed(b"one\ntw", |s| got.push(s.to_owned()));
        l.feed(b"o\nthree", |s| got.push(s.to_owned()));
        l.rest(|s| got.push(s.to_owned()));
        assert_eq!(got, ["one", "two", "three"]);
    }
}
