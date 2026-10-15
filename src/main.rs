mod app;

use std::path::PathBuf;

/// No library given or remembered: the app's own music folder (`$XDG_MUSIC_DIR`, else `~/Music/OpenFadeR`), created
/// if missing, opened as a folder of audio; recordings and downloads land there too.
fn own_folder() -> Option<PathBuf> {
    let dir = openfader::paths::output_dir("OPENFADER_MUSIC")?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    openfader::audio::device::quiet_alsa();
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(app::library::settings::database_path)
        .or_else(own_folder)
        .ok_or(
            "usage: openfader <library file | device root | folder of audio> (no HOME to fall back to)",
        )?;
    let result = app::run(&path);
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
