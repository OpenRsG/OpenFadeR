#[cfg(not(target_os = "android"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let music = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("usage: openfader-mobile <music-folder>")?;
    openfader_mobile::run(music)?;
    Ok(())
}

#[cfg(target_os = "android")]
compile_error!("Build the Android library with mobile/build-android.sh, not the preview binary");
