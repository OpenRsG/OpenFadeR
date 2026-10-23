fn main() {
    // ort's WebGPU provider (STEMS on the GPU) needs libwebgpu_dawn next to the binary, which ort's copy-dylibs puts there. Linux and macOS
    // look for it through an rpath; Windows loads a DLL from the executable's folder without one.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path"),
        _ => {}
    }
    // The commit shown in problem reports (docs/diagnostics.md): `$OPENFADER_COMMIT` (a build outside a git checkout), else git's.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let commit = std::env::var("OPENFADER_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "--short=9", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=OPENFADER_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=OPENFADER_COMMIT");
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    // Windows: the program's icon (assets/openfader.rc); a native build only, the dependency is the host's.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/openfader.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("cannot embed the program icon");
        println!("cargo:rerun-if-changed=assets/openfader.rc");
        println!("cargo:rerun-if-changed=assets/openfader.ico");
    }
    // Nothing else is read here: the UI and its generated helpers are the `openfader-ui` crate's (ui/build.rs), so a .slint
    // edit rebuilds that crate and the app, not the core library.
    println!("cargo:rerun-if-changed=build.rs");
}
