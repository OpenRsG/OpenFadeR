// A windowed program on Windows: no console behind the window (output still goes where the launcher redirected it).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;

use std::path::PathBuf;

/// The app's own music folder (`$XDG_MUSIC_DIR`, else `~/Music/OpenFadeR`), created if missing: always a connected folder;
/// recordings and downloads land there too.
fn own_folder() -> Option<PathBuf> {
    let dir = openfader::paths::output_dir("OPENFADER_MUSIC")?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// `openfader --rollback`: puts back the version an in-app update kept in `<install>.prev`, without opening a window (for a
/// new version that does not start). Prints the result.
fn roll_back() -> Result<(), Box<dyn std::error::Error>> {
    let install = openfader::update::roll_back()?;
    println!("rolled back: start {} again", install.exe_path().display());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == "--rollback")
    {
        return roll_back();
    }
    low_memory_malloc();
    openfader::diag::init(); // after the malloc settings (it starts threads): the log file and the crash reports
    openfader::audio::device::quiet_alsa();
    // A path given (or the old Database setting) is a library to connect; the own music folder always is one.
    let legacy = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(app::library::settings::database_path);
    let result = app::run(legacy, own_folder());
    openfader::stems::model::shutdown(); // Dawn (the GPU engine of STEMS) crashes in `exit()` with a session still open
    openfader::diag::finish(); // before the restart below replaces the process
    if result.is_ok() && app::restart_requested() {
        // The plugin manager's RESTART NOW: the same program with the same arguments.
        let exe = match app::restart_exe() {
            Some(exe) => exe.clone(),
            None => std::env::current_exe()?,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(std::env::args_os().skip(1));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            return Err(cmd.exec().into());
        }
        #[cfg(not(unix))]
        {
            cmd.spawn()?;
        }
    }
    result
}

/// glibc's malloc keeps what threads free: its mmap threshold grows to 32 MB after a big block is freed (so the next track's
/// buffers come from the heap and stay there) and every busy thread gets its own arena. Measured with two 6-min tracks
/// loaded: 274 MB RSS by default, 203 MB with these. A fixed threshold sends every block of 256 KiB or more to mmap (given
/// back on free); two arenas and trimming at 256 KiB return the rest. The audio thread does not allocate, so its timing
/// is unaffected.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn low_memory_malloc() {
    // SAFETY: called first in main, before any other thread exists.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 * 1024);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 256 * 1024);
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn low_memory_malloc() {}
