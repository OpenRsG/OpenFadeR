fn main() {
    // ort's WebGPU provider (STEMS on the GPU) needs libwebgpu_dawn.so, which ort's copy-dylibs puts next to the binary.
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    // Nothing else is read here: the UI and its generated helpers are the `openfader-ui` crate's (ui/build.rs), so a .slint
    // edit rebuilds that crate and the app, not the core library.
    println!("cargo:rerun-if-changed=build.rs");
}
