//! The sources of the connected libraries, watched for change (Linux, inotify): [`Watch`] says when something in them was written,
//! created, moved or deleted, so the live sync ([`super`]) reads their stamps then and not every few seconds. One thread blocks
//! in poll(2) on the inotify descriptor; a burst of events (a copy, a library program saving) is one report ([`Settle`]).
//!
//! inotify watches single folders, not trees: a connected folder gets a watch on every folder in it (down to the depth its stamp
//! looks at, at most [`MAX_DIRS`] in all; the rest is left to the fallback poll of [`super`]) and a library file one on its parent.
//! Folders created later are picked up when a burst ends. Where inotify is not there (other systems, no descriptors left)
//! [`Watch::start`] says no and [`super`] polls as it always did.

use std::path::PathBuf;

/// What to watch: a folder (and how many levels of folders below it), or the parent of a library file (depth 0).
pub type Root = (PathBuf, usize);

/// The folders to watch for `roots`: each root and the folders up to its depth below it, `cap` at most, each once.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn dirs(roots: &[Root], cap: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (root, depth) in roots {
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, level)) = stack.pop() {
            if out.len() >= cap {
                return out;
            }
            if !seen.insert(dir.clone()) {
                continue;
            }
            if level < *depth {
                for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push((e.path(), level + 1));
                    }
                }
            }
            out.push(dir);
        }
    }
    out
}

#[cfg(target_os = "linux")]
pub use linux::Watch;

#[cfg(target_os = "linux")]
mod linux {
    use super::{Root, dirs};
    use openfader::device::watch::Settle;
    use std::{
        io::{PipeReader, PipeWriter, Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{Receiver, Sender, channel},
        },
        time::{Duration, Instant},
    };

    /// Folders watched at most (the default limit of watches per user is 8192 and other programs use them too).
    const MAX_DIRS: usize = 4096;

    /// What counts as a change: content, metadata (a `touch`), and files and folders coming, going or moving.
    const MASK: u32 = libc::IN_MODIFY
        | libc::IN_ATTRIB
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;

    #[derive(Default)]
    struct Flags {
        changed: AtomicBool,
        /// The thread ended (it cannot be told about the change any more).
        dead: AtomicBool,
    }

    pub struct Watch {
        flags: Arc<Flags>,
        tx: Sender<Vec<Root>>,
        /// One byte per [`set`](Self::set) wakes the thread; closing it ends the thread.
        wake: PipeWriter,
        roots: Vec<Root>,
    }

    fn inotify() -> Option<OwnedFd> {
        // SAFETY: a plain syscall; the descriptor is owned by the OwnedFd.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        // SAFETY: `fd` is a new descriptor nobody else owns.
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A new inotify descriptor watching what `roots` name now (the old one, with its watches, is dropped by the caller).
    fn watching(roots: &[Root]) -> Option<OwnedFd> {
        let ino = inotify()?;
        for dir in dirs(roots, MAX_DIRS) {
            let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: `path` is a NUL-terminated string that outlives the call.
            let wd = unsafe { libc::inotify_add_watch(ino.as_raw_fd(), path.as_ptr(), MASK) };
            if wd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSPC) {
                break; // no watches left: the fallback poll covers the rest
            }
        }
        Some(ino)
    }

    /// Reads and drops the queued events; whether there were any.
    fn drain(ino: &OwnedFd) -> bool {
        let (mut buf, mut any) = ([0u8; 4096], false);
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::read(ino.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return any;
            }
            any = true;
        }
    }

    struct Ends(Arc<Flags>);

    impl Drop for Ends {
        fn drop(&mut self) {
            self.0.dead.store(true, Ordering::Relaxed);
        }
    }

    fn run(
        mut ino: OwnedFd,
        rx: Receiver<Vec<Root>>,
        mut wake: PipeReader,
        flags: Arc<Flags>,
        mut settle: Settle,
    ) {
        let _ends = Ends(flags.clone());
        let mut roots: Vec<Root> = Vec::new();
        loop {
            let now = Instant::now();
            let ms = if settle.pending() {
                settle
                    .wait(now, Duration::from_secs(60))
                    .as_millis()
                    .min(i32::MAX as u128 - 1) as i32
                    + 1
            } else {
                -1
            };
            let mut fds = [
                libc::pollfd {
                    fd: ino.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `fds` is valid for its length.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if fds[1].revents != 0 {
                if wake.read(&mut [0u8; 64]).is_ok_and(|n| n == 0)
                    || fds[1].revents & libc::POLLIN == 0
                {
                    return; // the Watch is gone
                }
                if let Some(latest) = rx.try_iter().last() {
                    roots = latest;
                    match watching(&roots) {
                        Some(i) => ino = i,
                        None => return,
                    }
                }
            }
            if fds[0].revents & libc::POLLIN != 0 && drain(&ino) {
                settle.hit(Instant::now());
            }
            if settle.fire(Instant::now()) {
                // folders created in the burst need watches of their own
                match watching(&roots) {
                    Some(i) => ino = i,
                    None => return,
                }
                flags.changed.store(true, Ordering::Relaxed);
            }
        }
    }

    impl Watch {
        /// Starts watching nothing (see [`set`](Self::set)); `None` without inotify.
        pub fn start() -> Option<Watch> {
            Self::start_with(Settle::new(Duration::from_secs(3), Duration::from_secs(30)))
        }

        fn start_with(settle: Settle) -> Option<Watch> {
            let ino = inotify()?;
            let (reader, wake) = std::io::pipe().ok()?;
            let (tx, rx) = channel();
            let flags = Arc::new(Flags::default());
            let f = flags.clone();
            std::thread::Builder::new()
                .name("connect-watch".into())
                .spawn(move || run(ino, rx, reader, f, settle))
                .ok()?;
            Some(Watch {
                flags,
                tx,
                wake,
                roots: Vec::new(),
            })
        }

        /// Watches `roots` from now on (nothing happens when they are the ones already watched).
        pub fn set(&mut self, roots: Vec<Root>) {
            if roots != self.roots && self.tx.send(roots.clone()).is_ok() {
                let _ = self.wake.write(&[0]);
                self.roots = roots;
            }
        }

        /// Whether the thread still runs: if not, nothing is reported any more.
        pub fn alive(&self) -> bool {
            !self.flags.dead.load(Ordering::Relaxed)
        }

        /// True once per settled burst of changes in what is watched.
        pub fn take(&self) -> bool {
            self.flags.changed.swap(false, Ordering::Relaxed)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_write_in_a_watched_folder_is_reported_once_it_settles() {
            let dir = std::env::temp_dir().join(format!("openfader-watch-{}", std::process::id()));
            let sub = dir.join("sub");
            std::fs::create_dir_all(&sub).unwrap();
            let Some(mut w) = Watch::start_with(Settle::new(
                Duration::from_millis(100),
                Duration::from_millis(500),
            )) else {
                return eprintln!("skip: no inotify");
            };
            w.set(vec![(dir.clone(), 1)]);
            std::thread::sleep(Duration::from_millis(300)); // the thread builds its watches
            assert!(!w.take(), "nothing happened yet");
            std::fs::write(sub.join("a.txt"), b"x").unwrap();
            let seen = (0..60).any(|_| {
                std::thread::sleep(Duration::from_millis(50));
                w.take()
            });
            assert!(seen, "the write in the subfolder was not reported");
            assert!(w.alive());
            drop(w);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub struct Watch;

#[cfg(not(target_os = "linux"))]
impl Watch {
    pub fn start() -> Option<Watch> {
        None
    }

    pub fn set(&mut self, _: Vec<Root>) {}

    pub fn alive(&self) -> bool {
        false
    }

    pub fn take(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("openfader-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("a/file.mp3"), b"x").unwrap();
        root
    }

    #[test]
    fn dirs_stop_at_the_depth_and_skip_files() {
        let root = tree();
        let sorted = |mut v: Vec<PathBuf>| {
            v.sort();
            v
        };
        assert_eq!(dirs(&[(root.clone(), 0)], 100), vec![root.clone()]);
        assert_eq!(
            sorted(dirs(&[(root.clone(), 1)], 100)),
            vec![root.clone(), root.join("a"), root.join("d")]
        );
        assert_eq!(dirs(&[(root.clone(), usize::MAX)], 100).len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dirs_respect_the_cap_and_list_a_folder_once() {
        let root = tree();
        assert_eq!(dirs(&[(root.clone(), usize::MAX)], 2).len(), 2);
        let twice = dirs(&[(root.join("a"), 0), (root.clone(), 1)], 100);
        assert_eq!(twice.iter().filter(|d| **d == root.join("a")).count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
