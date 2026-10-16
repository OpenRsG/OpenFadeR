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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    openfader::audio::device::quiet_alsa();
    // A path given (or the old Database setting) is a library to connect; the own music folder always is one.
    let legacy = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(app::library::settings::database_path);
    let result = app::run(legacy, own_folder());
    openfader::stems::model::shutdown(); // Dawn (the GPU engine of STEMS) crashes in `exit()` with a session still open
    if result.is_ok() && app::restart_requested() {
        // The plugin manager's RESTART NOW: the same program with the same arguments.
        let exe = std::env::current_exe()?;
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
