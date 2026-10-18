//! Plugin processes: every child process a plugin runs is started through [`spawn`], which ties it to the app, and a
//! `"persistent": true` plugin keeps one process for all its requests ([`call`]).
//!
//! No process outlives the app:
//! - Linux: the child gets `PR_SET_PDEATHSIG` = `SIGKILL`, so the kernel kills it however the app ends (crash, `kill -9`).
//!   The signal follows the thread that forked, so every spawn happens on one thread that lives as long as the app.
//! - Windows: the child goes into a job object that kills its processes when the job's last handle closes, which the
//!   system does when the app ends, however it ends.
//! - Every OS: the child's stdin is a pipe from the app; it reads end-of-file once the app is gone, and a persistent
//!   plugin must exit then (the only signal on macOS). On unix the child leads its own process group, so stopping it
//!   also stops what it started.
//! - A normal quit ([`stop_all`]) closes every persistent plugin's stdin, gives it [`GRACE`] to exit, then kills it.
//!   A process that dies on its own is reaped by its reader thread, never left a zombie.
//!
//! Persistent protocol: one JSON object per line both ways. Each request carries an `"id"` (a number); the reply carries
//! the same `id`, in any order, so requests may overlap. A line without `id` may carry `commands` (as an action's reply)
//! at any time, which the app runs. A request without a reply within its timeout kills the process; the next request
//! starts a new one.

use super::{action, event, Plugin};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::{Duration, Instant};

/// How long a persistent plugin has to exit after its stdin closes, at quit or when it is switched off.
pub const GRACE: Duration = Duration::from_millis(1000);

/// A child process tied to the app (see the module doc). Dropping it ends it and what it started ([`Owned::end`]).
/// Check it with [`Owned::running`], not `try_wait`: a reap frees the process group id before `end` can use it.
pub(super) struct Owned {
    child: Child,
    reaped: bool,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl Deref for Owned {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Owned {
    /// Whether the process still runs. Never reaps it, so its process group id stays ours until [`Owned::end`].
    pub(super) fn running(&mut self) -> bool {
        // SAFETY: waitid on our own child with WNOWAIT only looks; `info` is plain data, zero is valid.
        #[cfg(unix)]
        let running = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
            libc::waitid(libc::P_PID, self.child.id() as libc::id_t, &mut info, flags) == 0
                && info.si_pid() == 0
        };
        #[cfg(not(unix))]
        let running = matches!(self.child.try_wait(), Ok(None));
        running
    }

    /// Ends the process and whatever it started (its process group on unix, its job on Windows), then reaps it. The
    /// status is the process's own when it had already exited.
    pub(super) fn end(&mut self) -> io::Result<ExitStatus> {
        if !self.reaped {
            self.reaped = true;
            // Not reaped yet, so the group id is still ours even if the leader has exited.
            #[cfg(unix)]
            // SAFETY: a signal to our own child's group.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            #[cfg(windows)]
            drop(self.job.take());
            let _ = self.child.kill();
        }
        self.child.wait()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.end();
    }
}

/// Starts `cmd` tied to the app (see the module doc).
pub(super) fn spawn(mut cmd: Command) -> io::Result<Owned> {
    no_window(&mut cmd);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    #[cfg(target_os = "linux")]
    let child = linux::spawn(cmd)?;
    #[cfg(not(target_os = "linux"))]
    let child = cmd.spawn()?;
    Ok(Owned {
        #[cfg(windows)]
        job: job::Job::of(&child),
        child,
        reaped: false,
    })
}

/// On Windows, a console program started by the app (which has no console) would open a console window of its own.
pub(super) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, 0x0800_0000); // CREATE_NO_WINDOW
    let _ = cmd;
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::{mpsc, LazyLock};

    type Job = (Command, mpsc::Sender<io::Result<Child>>);

    /// The one thread that forks plugin processes: it never ends, so their parent-death signal fires only with the app.
    static SPAWNER: LazyLock<mpsc::Sender<Job>> = LazyLock::new(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("plugin-spawn".into())
            .spawn(move || {
                for (mut cmd, back) in rx {
                    let _ = back.send(cmd.spawn());
                }
            })
            .expect("plugin spawner thread");
        tx
    });

    pub fn spawn(mut cmd: Command) -> io::Result<Child> {
        let app = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != app {
                    libc::_exit(0); // the app ended before the signal was armed
                }
                Ok(())
            });
        }
        let (back, answer) = mpsc::channel();
        SPAWNER
            .send((cmd, back))
            .map_err(|_| io::Error::other("plugin spawner gone"))?;
        answer
            .recv()
            .map_err(|_| io::Error::other("plugin spawner gone"))?
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job holding one plugin process (and what it starts): closing it, or the app ending, kills them.
    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn of(child: &Child) -> Option<Job> {
            // SAFETY: plain Win32 calls on handles we own; the struct is plain data, zero is a valid value.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let job = Job(h);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
                    && AssignProcessToJobObject(h, child.as_raw_handle() as HANDLE) != 0;
                ok.then_some(job)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: our handle, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// One running persistent plugin.
struct Server {
    /// What it was started from: a request for a plugin whose folder, command or version changed restarts it.
    key: String,
    /// Lines for its stdin (a writer thread, so a plugin that stops reading cannot block a caller); `None` closes it.
    lines: Mutex<Option<mpsc::Sender<String>>>,
    /// Requests waiting for their reply, by `id`.
    pending: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    proc: Mutex<Owned>,
}

/// The running persistent plugins, by plugin name.
static SERVERS: LazyLock<Mutex<HashMap<String, Arc<Server>>>> = LazyLock::new(Default::default);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Sends `request` to `plugin`'s persistent process, started from `cmd` (`key` = what `cmd` is) unless it runs.
pub(super) fn call(
    plugin: &Plugin,
    key: String,
    cmd: Command,
    request: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let name = &plugin.manifest.name;
    let server = server(name, key, cmd)?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    server.pending.lock().insert(id, tx);
    let mut request = request.clone();
    if let Some(o) = request.as_object_mut() {
        o.insert("id".into(), id.into());
    }
    let sent = server
        .lines
        .lock()
        .as_ref()
        .is_some_and(|l| l.send(request.to_string() + "\n").is_ok());
    if !sent {
        server.pending.lock().remove(&id);
        return Err(format!("{name}: the plugin process is gone"));
    }
    match rx.recv_timeout(timeout) {
        Ok(reply) => match reply.get("error").and_then(Value::as_str) {
            Some(e) => Err(format!("{name}: {e}")),
            None => Ok(reply),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => {
            server.pending.lock().remove(&id);
            forget(name, &server);
            let _ = server.proc.lock().end();
            Err(format!("{name}: no reply within {} s", timeout.as_secs()))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{name}: the plugin process exited"))
        }
    }
}

/// `name`'s running server, (re)started when it is missing or `key` changed.
fn server(name: &str, key: String, cmd: Command) -> Result<Arc<Server>, String> {
    let mut all = SERVERS.lock();
    if let Some(s) = all.get(name) {
        if s.key == key {
            return Ok(s.clone());
        }
        let old = all.remove(name).expect("just seen");
        std::thread::spawn(move || stop(&[old]));
    }
    let mut proc = spawn(cmd).map_err(|e| format!("{name}: cannot start: {e}"))?;
    let (Some(mut stdin), Some(stdout)) = (proc.stdin.take(), proc.stdout.take()) else {
        return Err(format!("{name}: no pipes"));
    };
    let (lines, rx) = mpsc::channel::<String>();
    let server = Arc::new(Server {
        key,
        lines: Mutex::new(Some(lines)),
        pending: Mutex::default(),
        proc: Mutex::new(proc),
    });
    // Ends when `lines` closes (stdin then closes: end-of-file for the plugin) or the plugin stops reading.
    std::thread::spawn(move || {
        for line in rx {
            if stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let (s, plugin) = (server.clone(), name.to_owned());
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() {
                route(&plugin, &s, &line);
            }
        }
        // stdout closed: the plugin exited or is about to; fail what waits, reap it.
        s.pending.lock().clear();
        forget(&plugin, &s);
        let _ = s.proc.lock().end();
    });
    all.insert(name.to_owned(), server.clone());
    Ok(server)
}

/// One line from a plugin: a reply to its waiting request, or commands of its own.
fn route(name: &str, server: &Server, line: &str) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return eprintln!("plugin {name}: a line that is not JSON ({e})"),
    };
    if let Some(id) = v.get("id").and_then(Value::as_u64) {
        if let Some(tx) = server.pending.lock().remove(&id) {
            let _ = tx.send(v);
        }
        return;
    }
    match action::commands(name, &v) {
        Ok(c) if !c.is_empty() => event::deliver(name.to_owned(), c),
        Ok(_) => {}
        Err(e) => eprintln!("plugin {e}"),
    }
}

/// Takes `server` out of the running ones, if it is still `name`'s.
fn forget(name: &str, server: &Arc<Server>) {
    let mut all = SERVERS.lock();
    if all.get(name).is_some_and(|s| Arc::ptr_eq(s, server)) {
        all.remove(name);
    }
}

/// Closes the servers' stdin, waits up to [`GRACE`] for them to exit, kills the rest.
fn stop(servers: &[Arc<Server>]) {
    for s in servers {
        s.lines.lock().take();
    }
    let until = Instant::now() + GRACE;
    while Instant::now() < until && servers.iter().any(|s| s.proc.lock().running()) {
        std::thread::sleep(Duration::from_millis(20));
    }
    for s in servers {
        let _ = s.proc.lock().end();
    }
}

/// Stops every persistent plugin (see [`stop`]); blocks up to [`GRACE`]. For the app's quit.
pub fn stop_all() {
    let all: Vec<_> = SERVERS.lock().drain().map(|(_, s)| s).collect();
    stop(&all);
}

/// Stops, in the background, the persistent plugins not named in `keep` (switched off or removed).
pub(super) fn retain(keep: &[&str]) {
    let gone: Vec<_> = {
        let mut all = SERVERS.lock();
        let names: Vec<String> = all
            .keys()
            .filter(|n| !keep.contains(&n.as_str()))
            .cloned()
            .collect();
        names.iter().filter_map(|n| all.remove(n)).collect()
    };
    if !gone.is_empty() {
        std::thread::spawn(move || stop(&gone));
    }
}
