//! Waveform image composition uses Slint's renderer, never a second graphics context.
//! AUTO rejects software GL implementations; GPU explicitly allows those for diagnosis.
use crate::app::{App, DeckState, prefs::{self, view::WaveRenderer}};
use slint::{ComponentHandle, GraphicsAPI};
use std::cell::Cell;

thread_local! {
    static AVAILABLE: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static ENABLED: Cell<bool> = const { Cell::new(false) };
}

fn software(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "software", "swrast", "swiftshader", "gdi generic"]
        .iter().any(|s| name.contains(s))
}

fn selected(mode: WaveRenderer, available: bool, hardware: bool) -> bool {
    available && (mode == WaveRenderer::Gpu || (mode == WaveRenderer::Auto && hardware))
}

pub fn gpu() -> bool { ENABLED.get() }

pub fn apply(app: &App) {
    let (available, hardware) = AVAILABLE.get();
    let mode = prefs::with(|p| p.view.waveform_renderer);
    let enabled = selected(mode, available, hardware);
    if ENABLED.replace(enabled) != enabled {
        eprintln!("waveform renderer: {} (requested {mode:?})", if enabled { "GPU image compositor" } else { "CPU" });
    }
    app.global::<DeckState>().set_waveform_gpu(enabled);
}

/// Called only with the renderer's context current. No GL objects or function pointers outlive it.
pub fn setup(api: &GraphicsAPI, app: &App) {
    let hardware = match api {
        GraphicsAPI::NativeOpenGL { get_proc_address } => {
            let p = get_proc_address(c"glGetString");
            if p.is_null() {
                eprintln!("Slint renderer: femtovg ({api:?}), GL_RENDERER unavailable");
                false
            } else {
                // GL_RENDERER is a context-owned NUL-terminated string (OpenGL 2 / GLES 2 and newer).
                let name = unsafe {
                    let get: unsafe extern "system" fn(u32) -> *const std::ffi::c_char = std::mem::transmute(p);
                    let name = get(0x1f01);
                    if name.is_null() { None } else { Some(std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned()) }
                };
                eprintln!("Slint renderer: femtovg ({api:?}), GL_RENDERER={}", name.as_deref().unwrap_or("unknown"));
                name.is_some_and(|name| !software(&name))
            }
        }
        _ => {
            eprintln!("Slint renderer: {api:?}");
            true
        }
    };
    AVAILABLE.set((true, hardware));
    apply(app);
    if !gpu() { eprintln!("waveform renderer: CPU (no hardware compositor selected)"); }
}

pub fn teardown() {
    // The window is already mutably borrowed here; never touch Slint properties during destruction.
    AVAILABLE.set((false, false));
    ENABLED.set(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn teardown_discards_capabilities_without_accessing_a_window() {
        AVAILABLE.set((true, true));
        ENABLED.set(true);
        teardown();
        assert_eq!(AVAILABLE.get(), (false, false));
        assert!(!gpu());
    }
    #[test]
    fn renderer_setting_parses_and_rejects_unknown_modes() {
        use crate::app::prefs::Section;
        let mut v = prefs::view::View::default();
        assert_eq!(v.waveform_renderer, WaveRenderer::Auto);
        for (text, mode) in [("gpu", WaveRenderer::Gpu), ("cpu", WaveRenderer::Cpu), ("auto", WaveRenderer::Auto)] {
            assert_eq!(v.parse("waveform_renderer", text), Some(true));
            assert_eq!(v.waveform_renderer, mode);
        }
        assert_eq!(v.parse("waveform_renderer", "unknown"), Some(false));
        assert_eq!(v.waveform_renderer, WaveRenderer::Auto);
    }
    #[test]
    fn auto_requires_hardware_and_gpu_falls_back_without_a_graphics_api() {
        for mode in [WaveRenderer::Auto, WaveRenderer::Gpu, WaveRenderer::Cpu] {
            assert!(!selected(mode, false, false));
            assert!(!selected(mode, false, true));
        }
        assert!(!selected(WaveRenderer::Auto, true, false));
        assert!(selected(WaveRenderer::Auto, true, true));
        assert!(selected(WaveRenderer::Gpu, true, false));
        assert!(!selected(WaveRenderer::Cpu, true, true));
        for name in ["llvmpipe (LLVM 20)", "softpipe", "Google SwiftShader", "GDI Generic"] {
            assert!(software(name));
        }
        for name in ["V3D 7.1", "NVIDIA GeForce RTX 3090", "Apple M2"] { assert!(!software(name)); }
    }
}
