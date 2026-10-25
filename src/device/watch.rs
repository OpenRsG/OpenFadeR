//! Change notification for the device list: [`Watch`] says when the mounts or the plugged block devices may have changed, so the
//! app looks at them ([`super::scan`]) then and not every second.
//!
//! Linux: a thread blocks in poll(2) on `/proc/self/mountinfo` (the kernel raises `POLLPRI | POLLERR` when the mount table
//! changes, see proc(5)) and on a netlink socket of uevents (a stick plugged in that nothing mounted yet changes no mount, and the
//! udev database [`super::plug`] reads is complete only at udev's own event, the second group of that socket). Other systems have
//! no such signal here and are looked at every [`POLL`]. Everywhere a slow fallback ([`FALLBACK`]) covers what the signals do
//! not: a folder of `OPENFADER_DEVICE_ROOTS` (looked at as often as the old poll), the free space another program changed.
//!
//! [`Settle`] is the debounce shared with the other watchers (MIDI ports, connected libraries).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// How often a system without a change signal is looked at.
pub const POLL: Duration = Duration::from_secs(1);

/// The longest the signals may stay silent before the devices are looked at anyway.
pub const FALLBACK: Duration = Duration::from_secs(10);

/// A burst of events (a plug raises dozens) as one: fires once the events were quiet for `quiet`, or `max` after the first
/// of them when they never are.
#[derive(Clone, Debug)]
pub struct Settle {
    quiet: Duration,
    max: Duration,
    first: Option<Instant>,
    last: Instant,
}

impl Settle {
    pub fn new(quiet: Duration, max: Duration) -> Self {
        Self {
            quiet,
            max,
            first: None,
            last: Instant::now(),
        }
    }

    /// An event happened at `now`.
    /// A burst is waiting to fire.
    pub fn pending(&self) -> bool {
        self.due_at().is_some()
    }

    pub fn hit(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = now;
    }

    fn due_at(&self) -> Option<Instant> {
        self.first
            .map(|f| (self.last + self.quiet).min(f + self.max))
    }

    /// How long to block: until the burst settles, at most `idle` (the whole of it when nothing happened).
    pub fn wait(&self, now: Instant, idle: Duration) -> Duration {
        self.due_at()
            .map_or(idle, |d| d.saturating_duration_since(now).min(idle))
    }

    /// Whether the burst has settled at `now`; true once per burst.
    pub fn fire(&mut self, now: Instant) -> bool {
        let due = self.due_at().is_some_and(|d| d <= now);
        if due {
            self.first = None;
        }
        due
    }

    /// Forgets the burst (its work is done by other means).
    pub fn clear(&mut self) {
        self.first = None;
    }
}

/// The OS's side: blocks until something may have changed.
#[cfg(target_os = "linux")]
mod sys {
    use super::{Duration, Instant, POLL, Settle};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    pub struct Source {
        mounts: Option<File>,
        uevent: Option<OwnedFd>,
        settle: Settle,
    }

    /// A kernel uevent or a udev event about a block device: `ACTION=add\0DEVPATH=…\0SUBSYSTEM=block\0…`
    /// (udev's carries a binary header before the same properties).
    pub fn is_block_event(msg: &[u8]) -> bool {
        msg.split(|&b| b == 0).any(|p| p == b"SUBSYSTEM=block")
    }

    /// The uevent socket, listening to the kernel's events (group 1) and udev's (group 2); needs no privilege.
    fn uevent_socket() -> Option<OwnedFd> {
        // SAFETY: plain syscalls; the fd is owned (and closed) by the OwnedFd, or closed here on failure.
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_KOBJECT_UEVENT,
            );
            if fd < 0 {
                return None;
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut sa: libc::sockaddr_nl = std::mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            sa.nl_groups = 1 | 2;
            let bound = libc::bind(
                fd.as_raw_fd(),
                (&raw const sa).cast(),
                size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            );
            (bound == 0).then_some(fd)
        }
    }

    /// Reads every queued event; whether one concerned a block device (or some were lost).
    fn drain(fd: RawFd) -> bool {
        let (mut buf, mut hit) = ([0u8; 8192], false);
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            match n {
                0 => return hit,
                1.. => hit |= is_block_event(&buf[..n as usize]),
                _ if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOBUFS) => {
                    hit = true; // the queue overflowed: events are lost, look anyway
                }
                _ => return hit,
            }
        }
    }

    fn pollfd(fd: Option<RawFd>, events: libc::c_short) -> libc::pollfd {
        libc::pollfd {
            fd: fd.unwrap_or(-1), // poll(2) skips negative descriptors
            events,
            revents: 0,
        }
    }

    impl Source {
        pub fn open() -> Self {
            Source {
                mounts: File::open("/proc/self/mountinfo").ok(),
                uevent: uevent_socket(),
                settle: Settle::new(Duration::from_millis(300), Duration::from_secs(2)),
            }
        }

        /// Blocks until a change has settled or `idle` is over.
        pub fn wait(&mut self, idle: Duration) {
            if self.mounts.is_none() && self.uevent.is_none() {
                return std::thread::sleep(POLL); // no signal on this system: the old poll
            }
            let end = Instant::now() + idle;
            loop {
                let now = Instant::now();
                let left = end.saturating_duration_since(now);
                let ms = self
                    .settle
                    .wait(now, left)
                    .as_millis()
                    .min(i32::MAX as u128 - 1) as i32
                    + 1;
                let mut fds = [
                    pollfd(self.mounts.as_ref().map(AsRawFd::as_raw_fd), libc::POLLPRI),
                    pollfd(self.uevent.as_ref().map(AsRawFd::as_raw_fd), libc::POLLIN),
                ];
                // SAFETY: `fds` is valid for its length.
                let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
                let now = Instant::now();
                if n < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return std::thread::sleep(POLL);
                }
                if fds[0].revents != 0 {
                    // The file stays readable-with-news until it is read again from the start: that re-arms it.
                    if let Some(mut f) = self.mounts.as_ref() {
                        let _ = f.seek(SeekFrom::Start(0));
                        let _ = f.read_to_end(&mut Vec::new());
                    }
                    self.settle.hit(now);
                }
                if fds[1].revents & libc::POLLIN != 0 {
                    if let Some(fd) = &self.uevent {
                        if drain(fd.as_raw_fd()) {
                            self.settle.hit(now);
                        }
                    }
                }
                if self.settle.fire(now) {
                    return;
                }
                if now >= end {
                    self.settle.clear();
                    return;
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use super::{Duration, POLL};

    pub struct Source;

    impl Source {
        pub fn open() -> Self {
            Source
        }

        pub fn wait(&mut self, _: Duration) {
            std::thread::sleep(POLL);
        }
    }
}

/// Whether the devices may have changed since [`Watch::take`] last said so.
pub struct Watch {
    changed: Arc<AtomicBool>,
}

impl Watch {
    /// Starts the watching thread. The first [`take`](Self::take) answers true. The thread ends within [`FALLBACK`] of the drop.
    pub fn start() -> Watch {
        let changed = Arc::new(AtomicBool::new(true));
        let weak = Arc::downgrade(&changed);
        let pretend = std::env::var_os("OPENFADER_DEVICE_ROOTS").is_some_and(|v| !v.is_empty());
        let idle = if pretend { POLL } else { FALLBACK };
        let spawned = std::thread::Builder::new()
            .name("device-watch".into())
            .spawn(move || {
                let mut source = sys::Source::open();
                loop {
                    source.wait(idle);
                    let Some(flag) = weak.upgrade() else { return };
                    flag.store(true, Ordering::Relaxed);
                }
            });
        if let Err(e) = spawned {
            eprintln!("devices: change watcher not started ({e}); looking at 1 s"); // the flag below then stays set
            changed.store(true, Ordering::Relaxed);
        }
        Watch { changed }
    }

    /// True once per change (or fallback interval): time to [`super::scan`].
    pub fn take(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_fires_after_quiet_and_only_once() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut s = Settle::new(ms(300), ms(2000));
        assert!(!s.fire(t0), "nothing happened");
        assert_eq!(
            s.wait(t0, ms(10_000)),
            ms(10_000),
            "idle: the whole fallback"
        );
        s.hit(t0);
        s.hit(t0 + ms(200));
        assert!(!s.fire(t0 + ms(400)), "the burst goes on until 200 + 300");
        assert_eq!(s.wait(t0 + ms(400), ms(10_000)), ms(100));
        assert!(s.fire(t0 + ms(500)));
        assert!(!s.fire(t0 + ms(600)), "once per burst");
    }

    #[test]
    fn settle_gives_up_waiting_for_a_burst_that_never_ends() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut s = Settle::new(ms(300), ms(1000));
        for i in 0..20 {
            s.hit(t0 + ms(i * 100)); // an event every 100 ms for 2 s
        }
        assert!(
            s.fire(t0 + ms(1000)),
            "1 s after the first, not 300 ms after the last"
        );
    }

    #[test]
    fn settle_wait_never_exceeds_idle() {
        let t0 = Instant::now();
        let mut s = Settle::new(Duration::from_secs(5), Duration::from_secs(5));
        s.hit(t0);
        assert_eq!(s.wait(t0, Duration::from_secs(1)), Duration::from_secs(1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn block_events_are_told_from_the_rest() {
        use sys::is_block_event;
        let kernel = b"add@/devices/pci0000:00/usb1/1-1/1-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb/sdb1\0ACTION=add\0DEVPATH=/devices/x/block/sdb/sdb1\0SUBSYSTEM=block\0DEVTYPE=partition\0";
        assert!(is_block_event(kernel));
        let mut udev = b"libudev\0\xfe\xed\xca\xfe".to_vec();
        udev.extend_from_slice(b"ACTION=remove\0SUBSYSTEM=block");
        assert!(
            is_block_event(&udev),
            "the last property needs no terminator"
        );
        assert!(!is_block_event(b"ACTION=add\0SUBSYSTEM=input\0"));
        assert!(!is_block_event(b"ACTION=add\0SUBSYSTEM=blockish\0"));
    }
}
