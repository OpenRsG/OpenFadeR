fn main() {
    // ort's WebGPU provider (STEMS on the GPU) needs libwebgpu_dawn next to the binary, which ort's copy-dylibs puts there. Linux and macOS
    // look for it through an rpath; Windows loads a DLL from the executable's folder without one.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path"),
        _ => {}
    }
    // Nothing else is read here: the UI and its generated helpers are the `openfader-ui` crate's (ui/build.rs), so a .slint
    // edit rebuilds that crate and the app, not the core library.
    println!("cargo:rerun-if-changed=build.rs");
}
